//! Renders, submits, monitors, adopts, and cancels exact deterministic Nomad batch jobs.

use std::fs;
use std::io::{self, Read};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Certificate, Identity};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::backend::{BuildExecution, BuildResult, BuildStatus, OutputTrust};
use crate::service::config::{NomadBackendConfig, NomadConstraint, NomadTransferAuthentication};

const MAXIMUM_NOMAD_RESPONSE_BYTES: u64 = 1024 * 1024;
const NOMAD_RETRY_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
const NOMAD_RETRY_MAXIMUM_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

pub struct NomadClient {
    config: NomadBackendConfig,
    client: Client,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NomadExecutionState {
    Pending,
    Placed,
    Succeeded,
    Failed,
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NomadSubmission {
    job_id: String,
    evaluation_id: String,
}

impl NomadSubmission {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub fn evaluation_id(&self) -> &str {
        &self.evaluation_id
    }
}

#[derive(Deserialize)]
struct SubmissionResponse {
    #[serde(rename = "EvalID")]
    eval_id: String,
}

#[derive(Deserialize)]
struct JobResponse {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Namespace")]
    namespace: String,
    #[serde(rename = "Type")]
    job_type: String,
    #[serde(rename = "Version", default)]
    version: u64,
    #[serde(rename = "Meta")]
    meta: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct AllocationResponse {
    #[serde(rename = "JobVersion", default)]
    job_version: u64,
    #[serde(rename = "ClientStatus")]
    client_status: String,
}

#[derive(Deserialize)]
struct ExactAllocationResponse {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Namespace")]
    namespace: String,
    #[serde(rename = "JobID")]
    job_id: String,
    #[serde(rename = "TaskGroup")]
    task_group: String,
    #[serde(rename = "ClientStatus")]
    client_status: String,
    #[serde(rename = "TaskStates", default)]
    task_states: std::collections::HashMap<String, AllocationTaskState>,
}

#[derive(Deserialize)]
struct AllocationTaskState {
    #[serde(rename = "State")]
    state: String,
}

impl NomadClient {
    pub fn new(config: NomadBackendConfig) -> io::Result<Self> {
        let mut headers = HeaderMap::new();
        if let Some(path) = config.token_file() {
            let token = fs::read_to_string(path)
                .map_err(|_| io::Error::other("Nomad client configuration failed"))?;
            let token = token.trim();
            if token.is_empty() {
                return Err(io::Error::other("Nomad client configuration failed"));
            }
            headers.insert(
                "X-Nomad-Token",
                HeaderValue::from_str(token)
                    .map_err(|_| io::Error::other("Nomad client configuration failed"))?,
            );
        }
        let mut builder = Client::builder()
            .default_headers(headers)
            .timeout(config.runtime_limit());
        if let Some(path) = config.ca_certificate_file() {
            let pem = fs::read(path)
                .map_err(|_| io::Error::other("Nomad client configuration failed"))?;
            let certificates = Certificate::from_pem_bundle(&pem)
                .map_err(|_| io::Error::other("Nomad client configuration failed"))?;
            if certificates.is_empty() {
                return Err(io::Error::other("Nomad client configuration failed"));
            }
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        if let (Some(certificate_path), Some(key_path)) =
            (config.client_certificate_file(), config.client_key_file())
        {
            let mut pem = fs::read(certificate_path)
                .map_err(|_| io::Error::other("Nomad client configuration failed"))?;
            pem.extend_from_slice(
                &fs::read(key_path)
                    .map_err(|_| io::Error::other("Nomad client configuration failed"))?,
            );
            builder = builder.identity(
                Identity::from_pem(&pem)
                    .map_err(|_| io::Error::other("Nomad client configuration failed"))?,
            );
        }
        let client = builder
            .build()
            .map_err(|_| io::Error::other("Nomad client configuration failed"))?;
        Ok(Self { config, client })
    }

    pub fn verify_allocation(
        &self,
        allocation_id: &str,
        job_id: &str,
        task: &str,
    ) -> io::Result<()> {
        if !valid_nomad_identity(allocation_id) {
            return Err(io::Error::other(format!(
                "Nomad allocation verification failed: invalid allocation ID {allocation_id:?}"
            )));
        }
        if !valid_nomad_identity(job_id) {
            return Err(io::Error::other(format!(
                "Nomad allocation verification failed: invalid job ID {job_id:?}"
            )));
        }
        if !valid_nomad_identity(task) {
            return Err(io::Error::other(format!(
                "Nomad allocation verification failed: invalid task {task:?}"
            )));
        }
        let response = self
            .client
            .get(format!(
                "{}/v1/allocation/{allocation_id}",
                self.config.endpoint()
            ))
            .query(&[("namespace", self.config.namespace())])
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| {
                io::Error::other(format!("Nomad allocation verification failed: {error}"))
            })?;
        let bytes = bounded_response(response, "Nomad allocation verification failed")?;
        let allocation: ExactAllocationResponse =
            serde_json::from_slice(&bytes).map_err(|error| {
                io::Error::other(format!("Nomad allocation verification failed: {error}"))
            })?;
        if allocation.id != allocation_id {
            return Err(io::Error::other("Nomad allocation ID verification failed"));
        }
        if allocation.namespace != self.config.namespace() {
            return Err(io::Error::other(
                "Nomad allocation namespace verification failed",
            ));
        }
        if allocation.job_id != job_id {
            return Err(io::Error::other("Nomad allocation job verification failed"));
        }
        if allocation.task_group != "build" {
            return Err(io::Error::other(
                "Nomad allocation task group verification failed",
            ));
        }
        if !matches!(allocation.client_status.as_str(), "pending" | "running") {
            return Err(io::Error::other(
                "Nomad allocation status verification failed",
            ));
        }
        if allocation.task_states.is_empty() {
            return Ok(());
        }
        let task_state = allocation
            .task_states
            .get(task)
            .ok_or_else(|| io::Error::other("Nomad allocation task verification failed"))?;
        tracing::debug!(
            event = "nomad.allocation.verified",
            task_state = task_state.state,
            "Nomad allocation verified"
        );
        Ok(())
    }

    pub fn status(&self, job_id: &str) -> io::Result<NomadExecutionState> {
        let started = Instant::now();
        tracing::trace!(
            event = "nomad.api.request.started",
            operation = "status",
            backend_name = self.config.target().name(),
            "Nomad API request started"
        );
        if job_id.is_empty() || job_id.len() > 256 {
            return Err(io::Error::other("Nomad job monitoring failed"));
        }
        let response = self
            .client
            .get(format!("{}/v1/job/{job_id}", self.config.endpoint()))
            .query(&[("namespace", self.config.namespace())])
            .send()
            .map_err(|_| io::Error::other("Nomad job monitoring failed"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            tracing::trace!(
                event = "nomad.api.request.completed",
                operation = "status",
                backend_name = self.config.target().name(),
                result = "missing",
                duration_ms = started.elapsed().as_millis(),
                "Nomad API request completed"
            );
            return Ok(NomadExecutionState::Missing);
        }
        let job: JobResponse = bounded_json(
            response
                .error_for_status()
                .map_err(|_| io::Error::other("Nomad job monitoring failed"))?,
            "Nomad job monitoring failed",
        )?;
        if job.id != job_id
            || job.namespace != self.config.namespace()
            || job.job_type != "batch"
            || job.meta.get("telchar_backend").map(String::as_str)
                != Some(self.config.target().name())
            || job.meta.get("telchar_system").map(String::as_str)
                != Some(self.config.target().system())
        {
            return Err(io::Error::other("Nomad job monitoring failed"));
        }
        let allocations: Vec<AllocationResponse> = bounded_json(
            self.client
                .get(format!(
                    "{}/v1/job/{job_id}/allocations",
                    self.config.endpoint()
                ))
                .query(&[("namespace", self.config.namespace())])
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .map_err(|_| io::Error::other("Nomad job monitoring failed"))?,
            "Nomad job monitoring failed",
        )?;
        let current_allocations = allocations
            .iter()
            .filter(|allocation| allocation.job_version == job.version)
            .collect::<Vec<_>>();
        let state = if current_allocations.is_empty() {
            NomadExecutionState::Pending
        } else if current_allocations
            .iter()
            .any(|allocation| allocation.client_status == "failed")
        {
            NomadExecutionState::Failed
        } else if current_allocations
            .iter()
            .all(|allocation| allocation.client_status == "complete")
        {
            NomadExecutionState::Succeeded
        } else {
            NomadExecutionState::Placed
        };
        tracing::trace!(
            event = "nomad.api.request.completed",
            operation = "status",
            backend_name = self.config.target().name(),
            result = ?state,
            allocation_count = allocations.len(),
            duration_ms = started.elapsed().as_millis(),
            "Nomad API request completed"
        );
        Ok(state)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        &self,
        database_url: &str,
        execution: &BuildExecution<'_>,
        shared_build_key: &[u8],
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        shared_builds: &crate::shared_build::SharedBuildRegistry,
        live_log_queue_bytes: usize,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        let profile = self
            .config
            .select_resource_profile(execution.build().required_system_features())
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Nomad resource profile selection is ambiguous",
                )
            })?;
        let started = Instant::now();
        let deadline = started.checked_add(execution.timeout()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "Nomad job execution timed out")
        })?;
        let shared_build_key_text = std::str::from_utf8(shared_build_key)
            .map_err(|_| io::Error::other("Nomad shared build key is invalid"))?;
        let derivation_path = std::str::from_utf8(execution.build().derivation_path())
            .map_err(|_| io::Error::other("Nomad derivation path is invalid"))?;
        let mut live_logs = shared_builds
            .subscribe_logs(shared_build_key_text, live_log_queue_bytes)
            .ok_or_else(|| io::Error::other("Nomad shared build live logs are unavailable"))?;
        let mut attempt_ordinal = 1_usize;
        let result = loop {
            let result = self.execute_attempt(
                database_url,
                execution,
                shared_build_key,
                attempt_ordinal,
                deadline,
                &profile,
                logs,
                &mut live_logs,
                cancelled,
            );
            match result {
                Ok(result) => break Ok(result),
                Err(NomadAttemptFailure::Terminal(error)) => break Err(error),
                Err(NomadAttemptFailure::Retryable(error))
                    if attempt_ordinal <= self.config.max_retries() =>
                {
                    let next_ordinal = attempt_ordinal + 1;
                    let current_execution_id = deterministic_job_name_for_attempt(
                        &self.config,
                        shared_build_key,
                        attempt_ordinal,
                    )?;
                    let next_execution_id = deterministic_job_name_for_attempt(
                        &self.config,
                        shared_build_key,
                        next_ordinal,
                    )?;
                    crate::persistence::retry_shared_build(
                        database_url,
                        derivation_path,
                        &current_execution_id,
                        &next_execution_id,
                        "nomad-infrastructure-lost",
                        &serde_json::json!({"reason": error.to_string()}),
                    )
                    .map_err(|_| io::Error::other("Nomad shared build retry failed"))?;
                    wait_for_retry(retry_delay(attempt_ordinal), deadline, cancelled)?;
                    attempt_ordinal = next_ordinal;
                }
                Err(NomadAttemptFailure::Retryable(error)) => break Err(error),
            }
        };
        for chunk in live_logs.drain() {
            logs(&chunk)?;
        }
        let result_name = if result.is_ok() {
            "succeeded"
        } else {
            "failed"
        };
        crate::service::metrics::nomad_execution_finished(
            self.config.target().name(),
            started.elapsed(),
            result_name,
        );
        tracing::debug!(
            event = "nomad.execution.completed",
            backend_name = self.config.target().name(),
            result = result_name,
            duration_ms = started.elapsed().as_millis(),
            "Nomad execution completed"
        );
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_attempt(
        &self,
        database_url: &str,
        execution: &BuildExecution<'_>,
        shared_build_key: &[u8],
        attempt_ordinal: usize,
        deadline: Instant,
        profile: &crate::service::config::SelectedNomadResourceProfile<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        live_logs: &mut crate::shared_build::SharedBuildLogReceiver,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> Result<BuildResult, NomadAttemptFailure> {
        if Instant::now() >= deadline {
            return Err(NomadAttemptFailure::Terminal(io::Error::new(
                io::ErrorKind::TimedOut,
                "Nomad job execution timed out",
            )));
        }
        let submission_started = Instant::now();
        let submission = self
            .submit_for_features_at_attempt(
                shared_build_key,
                execution.build().required_system_features(),
                attempt_ordinal,
            )
            .map_err(NomadAttemptFailure::Retryable);
        crate::service::metrics::nomad_submission_finished(
            self.config.target().name(),
            profile.name(),
            profile.priority().default(),
            submission_started.elapsed(),
            if submission.is_ok() {
                "succeeded"
            } else {
                "failed"
            },
        );
        let submission = submission?;
        crate::service::metrics::nomad_pending_changed(self.config.target().name(), 1);
        let attempt_started = Instant::now();
        let result = (|| {
            let mut placement_recorded = false;
            loop {
                for chunk in live_logs.drain() {
                    logs(&chunk).map_err(NomadAttemptFailure::Terminal)?;
                }
                if cancelled().map_err(NomadAttemptFailure::Terminal)? {
                    self.stop(submission.job_id())
                        .map_err(NomadAttemptFailure::Terminal)?;
                    return Err(NomadAttemptFailure::Terminal(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "Nomad job execution cancelled",
                    )));
                }
                if Instant::now() >= deadline {
                    self.stop(submission.job_id())
                        .map_err(NomadAttemptFailure::Terminal)?;
                    return Err(NomadAttemptFailure::Terminal(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Nomad job execution timed out",
                    )));
                }
                let build = crate::persistence::read_shared_build(
                    database_url,
                    std::str::from_utf8(execution.build().derivation_path()).map_err(|_| {
                        NomadAttemptFailure::Terminal(io::Error::other(
                            "Nomad derivation path is invalid",
                        ))
                    })?,
                )
                .map_err(|_| {
                    NomadAttemptFailure::Terminal(io::Error::other(
                        "Nomad shared build state is unavailable",
                    ))
                })?
                .ok_or_else(|| {
                    NomadAttemptFailure::Terminal(io::Error::other(
                        "Nomad shared build is unavailable",
                    ))
                })?;
                match build.state {
                    crate::persistence::SharedBuildState::Succeeded => {
                        return BuildResult::new(
                            BuildStatus::Built,
                            execution.build().expected_outputs().to_vec(),
                            OutputTrust::TrustedExecutor,
                        )
                        .map_err(NomadAttemptFailure::Terminal);
                    }
                    crate::persistence::SharedBuildState::Failed => {
                        return Err(NomadAttemptFailure::Terminal(io::Error::other(
                            "Nomad build transfer failed",
                        )));
                    }
                    crate::persistence::SharedBuildState::Claimed
                    | crate::persistence::SharedBuildState::Running
                    | crate::persistence::SharedBuildState::Collecting => {}
                }
                match self
                    .status(submission.job_id())
                    .map_err(NomadAttemptFailure::Retryable)?
                {
                    NomadExecutionState::Pending => {}
                    NomadExecutionState::Placed | NomadExecutionState::Succeeded => {
                        if !placement_recorded {
                            crate::service::metrics::nomad_placed(
                                self.config.target().name(),
                                attempt_started.elapsed(),
                            );
                            placement_recorded = true;
                        }
                    }
                    NomadExecutionState::Failed | NomadExecutionState::Missing => {
                        return Err(NomadAttemptFailure::Retryable(io::Error::other(
                            "Nomad job execution failed",
                        )));
                    }
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                for chunk in live_logs.wait_and_drain(self.config.poll_interval().min(remaining)) {
                    logs(&chunk).map_err(NomadAttemptFailure::Terminal)?;
                }
            }
        })();
        crate::service::metrics::nomad_pending_changed(self.config.target().name(), -1);
        result
    }

    fn stop(&self, job_id: &str) -> io::Result<()> {
        let started = Instant::now();
        tracing::trace!(
            event = "nomad.api.request.started",
            operation = "stop",
            backend_name = self.config.target().name(),
            "Nomad API request started"
        );
        if !valid_nomad_identity(job_id) {
            return Err(io::Error::other("Nomad job cancellation failed"));
        }
        bounded_json::<serde_json::Value>(
            self.client
                .delete(format!("{}/v1/job/{job_id}", self.config.endpoint()))
                .query(&[("namespace", self.config.namespace()), ("purge", "true")])
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .map_err(|_| io::Error::other("Nomad job cancellation failed"))?,
            "Nomad job cancellation failed",
        )?;
        tracing::trace!(
            event = "nomad.api.request.completed",
            operation = "stop",
            backend_name = self.config.target().name(),
            result = "succeeded",
            duration_ms = started.elapsed().as_millis(),
            "Nomad API request completed"
        );
        Ok(())
    }

    pub fn submit(&self, shared_build_key: &[u8]) -> io::Result<NomadSubmission> {
        self.submit_for_attempt(shared_build_key, 1)
    }

    pub fn submit_for_attempt(
        &self,
        shared_build_key: &[u8],
        attempt_ordinal: usize,
    ) -> io::Result<NomadSubmission> {
        self.submit_for_features_at_attempt(shared_build_key, &[] as &[&str], attempt_ordinal)
    }

    pub fn submit_for_features<S: AsRef<str>>(
        &self,
        shared_build_key: &[u8],
        required_features: &[S],
    ) -> io::Result<NomadSubmission> {
        self.submit_for_features_at_attempt(shared_build_key, required_features, 1)
    }

    pub fn submit_for_features_at_attempt<S: AsRef<str>>(
        &self,
        shared_build_key: &[u8],
        required_features: &[S],
        attempt_ordinal: usize,
    ) -> io::Result<NomadSubmission> {
        let started = Instant::now();
        tracing::trace!(
            event = "nomad.api.request.started",
            operation = "submit",
            backend_name = self.config.target().name(),
            "Nomad API request started"
        );
        let job_id =
            deterministic_job_name_for_attempt(&self.config, shared_build_key, attempt_ordinal)?;
        let response = self
            .client
            .post(format!("{}/v1/jobs", self.config.endpoint()))
            .query(&[("namespace", self.config.namespace())])
            .json(&render_job_for_features_at_attempt(
                &self.config,
                shared_build_key,
                required_features,
                attempt_ordinal,
            )?)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|_| io::Error::other("Nomad job submission failed"))?;
        let parsed: SubmissionResponse = bounded_json(response, "Nomad job submission failed")?;
        if parsed.eval_id.is_empty() || parsed.eval_id.len() > 256 {
            return Err(io::Error::other("Nomad job submission failed"));
        }
        tracing::trace!(
            event = "nomad.api.request.completed",
            operation = "submit",
            backend_name = self.config.target().name(),
            result = "succeeded",
            duration_ms = started.elapsed().as_millis(),
            "Nomad API request completed"
        );
        Ok(NomadSubmission {
            job_id,
            evaluation_id: parsed.eval_id,
        })
    }
}

enum NomadAttemptFailure {
    Retryable(io::Error),
    Terminal(io::Error),
}

fn retry_delay(attempt_ordinal: usize) -> std::time::Duration {
    let exponent = u32::try_from(attempt_ordinal.saturating_sub(1).min(16)).unwrap_or(16);
    let base = NOMAD_RETRY_INITIAL_DELAY
        .checked_mul(2_u32.saturating_pow(exponent))
        .unwrap_or(NOMAD_RETRY_MAXIMUM_DELAY)
        .min(NOMAD_RETRY_MAXIMUM_DELAY);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos());
    let jitter_millis = u64::from(nanos % 100);
    base.saturating_add(std::time::Duration::from_millis(jitter_millis))
        .min(NOMAD_RETRY_MAXIMUM_DELAY)
}

fn wait_for_retry(
    delay: std::time::Duration,
    deadline: Instant,
    cancelled: &mut dyn FnMut() -> io::Result<bool>,
) -> io::Result<()> {
    let wake = Instant::now()
        .checked_add(delay)
        .unwrap_or(deadline)
        .min(deadline);
    while Instant::now() < wake {
        if cancelled()? {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Nomad job execution cancelled",
            ));
        }
        std::thread::sleep(
            std::time::Duration::from_millis(25)
                .min(wake.saturating_duration_since(Instant::now())),
        );
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Nomad job execution timed out",
        ));
    }
    Ok(())
}

fn valid_nomad_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn bounded_response(
    response: reqwest::blocking::Response,
    failure: &'static str,
) -> io::Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAXIMUM_NOMAD_RESPONSE_BYTES)
    {
        return Err(io::Error::other(failure));
    }
    let mut bytes = Vec::new();
    response
        .take(MAXIMUM_NOMAD_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| io::Error::other(failure))?;
    if bytes.len() as u64 > MAXIMUM_NOMAD_RESPONSE_BYTES {
        return Err(io::Error::other(failure));
    }
    Ok(bytes)
}

fn bounded_json<T: serde::de::DeserializeOwned>(
    response: reqwest::blocking::Response,
    failure: &'static str,
) -> io::Result<T> {
    let bytes = bounded_response(response, failure)?;
    serde_json::from_slice(&bytes).map_err(|_| io::Error::other(failure))
}

pub fn deterministic_job_name(config: &NomadBackendConfig, shared_build_key: &[u8]) -> String {
    deterministic_job_name_for_attempt(config, shared_build_key, 1)
        .expect("initial Nomad attempt ordinal is valid")
}

pub fn deterministic_job_name_for_attempt(
    config: &NomadBackendConfig,
    shared_build_key: &[u8],
    attempt_ordinal: usize,
) -> io::Result<String> {
    if attempt_ordinal == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Nomad attempt ordinal is invalid",
        ));
    }
    let digest = Sha256::digest(shared_build_key);
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!(
        "{}-{suffix}-{attempt_ordinal}",
        config.job_name_scope()
    ))
}

pub fn render_job(config: &NomadBackendConfig, shared_build_key: &[u8]) -> io::Result<Value> {
    render_job_for_features(config, shared_build_key, &[] as &[&str])
}

pub fn render_job_for_features<S: AsRef<str>>(
    config: &NomadBackendConfig,
    shared_build_key: &[u8],
    required_features: &[S],
) -> io::Result<Value> {
    render_job_for_features_at_attempt(config, shared_build_key, required_features, 1)
}

pub fn render_job_for_features_at_attempt<S: AsRef<str>>(
    config: &NomadBackendConfig,
    shared_build_key: &[u8],
    required_features: &[S],
    attempt_ordinal: usize,
) -> io::Result<Value> {
    render_job_at(
        config,
        shared_build_key,
        required_features,
        attempt_ordinal,
        SystemTime::now(),
    )
}

fn render_job_at<S: AsRef<str>>(
    config: &NomadBackendConfig,
    shared_build_key: &[u8],
    required_features: &[S],
    attempt_ordinal: usize,
    issued_at: SystemTime,
) -> io::Result<Value> {
    let job_id = deterministic_job_name_for_attempt(config, shared_build_key, attempt_ordinal)?;
    let profile = config
        .select_resource_profile(required_features)
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Nomad resource profile selection is ambiguous",
            )
        })?;
    tracing::debug!(
        event = "nomad.resource_profile.selected",
        backend_name = config.target().name(),
        resource_profile = profile.name(),
        priority = profile.priority().default(),
        cpu_mhz = profile.resources().cpu_mhz(),
        memory_mb = profile.resources().memory_mb(),
        disk_mb = profile.resources().disk_mb(),
        "Nomad resource profile selected"
    );
    let mut task = json!({
        "Name": "build",
        "Driver": config.driver(),
        "Config": Value::Object(config.driver_config().clone()),
        "Resources": {
            "CPU": profile.resources().cpu_mhz(),
            "MemoryMB": profile.resources().memory_mb(),
            "DiskMB": profile.resources().disk_mb(),
        },
        "Env": {
            "TELCHAR_TRANSFER_ENDPOINT": config.transfer_endpoint(),
            "TELCHAR_NIX_STORE_URI": config.store().uri(),
            "TELCHAR_TRANSFER_CHUNK_BYTES": config.transfer_limits().stream_buffer_bytes().to_string(),
            "TELCHAR_MAXIMUM_MANIFEST_BYTES": config.transfer_limits().maximum_manifest_bytes().to_string(),
            "TELCHAR_TRANSFER_IDLE_TIMEOUT_SECONDS": config.transfer_limits().transfer_idle_timeout().as_secs().to_string(),
            "TELCHAR_SETUP_TIMEOUT_SECONDS": config.transfer_limits().setup_timeout().as_secs().to_string(),
            "TELCHAR_OUTPUT_COLLECTION_TIMEOUT_SECONDS": config.transfer_limits().output_collection_timeout().as_secs().to_string(),
            "TELCHAR_MAXIMUM_CONNECTION_LIFETIME_SECONDS": config.transfer_limits().maximum_connection_lifetime().as_secs().to_string(),
            "TELCHAR_MAXIMUM_DIAGNOSTIC_BYTES": config.transfer_limits().maximum_diagnostic_bytes().to_string(),
        },
    });
    match config.transfer_authentication() {
        NomadTransferAuthentication::WorkloadIdentity { .. } => {
            task["Env"]["TELCHAR_TRANSFER_AUTHENTICATION"] = Value::from("workload-identity");
            task["Env"]["TELCHAR_BACKEND"] = Value::from(config.target().name());
            task["Env"]["TELCHAR_NAMESPACE"] = Value::from(config.namespace());
            task["Env"]["TELCHAR_JOB_ID"] = Value::from(job_id.clone());
            task["Env"]["TELCHAR_SHARED_BUILD_DIGEST"] =
                Value::from(URL_SAFE_NO_PAD.encode(Sha256::digest(shared_build_key)));
            task["Env"]["TELCHAR_TASK"] = Value::from("build");
            task["Identities"] = json!([{
                "Name": "telchar_transfer",
                "Env": true,
                "File": false,
                "Audience": [config
                    .transfer_authentication()
                    .audience()
                    .expect("workload identity audience is configured")],
                "TTL": 3_600_000_000_000_u64,
            }]);
        }
        NomadTransferAuthentication::Hmac {
            key_id,
            secret_file,
        } => {
            task["Env"]["TELCHAR_TRANSFER_AUTHENTICATION"] = Value::from("hmac");
            task["Env"]["TELCHAR_TRANSFER_CAPABILITY"] = Value::from(hmac_capability(
                config,
                shared_build_key,
                key_id,
                secret_file,
                issued_at,
            )?);
        }
    }
    let mut tasks = Vec::with_capacity(2);
    if let Some(prestart) = config.prestart() {
        tasks.push(json!({
            "Name": "prestart",
            "Driver": prestart.driver(),
            "Config": Value::Object(prestart.driver_config().clone()),
            "Resources": {
                "CPU": prestart.resources().cpu_mhz(),
                "MemoryMB": prestart.resources().memory_mb(),
                "DiskMB": prestart.resources().disk_mb(),
            },
            "Lifecycle": {
                "Hook": "prestart",
                "Sidecar": false,
            },
            "KillTimeout": duration_nanoseconds(prestart.timeout()),
        }));
    }
    tasks.push(task);
    let mut group = Map::new();
    group.insert("Name".to_owned(), Value::String("build".to_owned()));
    group.insert("Count".to_owned(), Value::from(1));
    group.insert(
        "RestartPolicy".to_owned(),
        json!({
            "Attempts": 0,
            "Mode": "fail",
        }),
    );
    group.insert(
        "ReschedulePolicy".to_owned(),
        json!({
            "Attempts": 0,
            "Unlimited": false,
        }),
    );
    group.insert("Tasks".to_owned(), Value::Array(tasks));
    if let Some(connect) = config.callback_connect() {
        group.insert("Networks".to_owned(), json!([{ "Mode": "bridge" }]));
        let sidecar_service = json!({
            "Proxy": {
                "Upstreams": [{
                    "DestinationName": connect.destination_service(),
                    "LocalBindPort": connect.local_bind_port(),
                }],
            },
        });
        let mut connect_config = json!({
            "SidecarService": sidecar_service,
        });
        if let Some(image) = connect.sidecar_image() {
            connect_config["SidecarTask"] = json!({
                "Config": {
                    "image": image,
                },
            });
        }
        group.insert(
            "Services".to_owned(),
            json!([{
                "Name": connect.source_service(),
                "Connect": connect_config,
            }]),
        );
    }
    let constraints = config
        .constraints()
        .iter()
        .chain(profile.constraints())
        .map(render_constraint)
        .collect::<Vec<_>>();
    Ok(json!({
        "Job": {
            "ID": job_id,
            "Name": job_id,
            "Type": "batch",
            "Namespace": config.namespace(),
            "NodePool": config.node_pool(),
            "Datacenters": ["*"],
            "Priority": profile.priority().default(),
            "Constraints": constraints,
            "TaskGroups": [Value::Object(group)],
            "Meta": {
                "telchar_backend": config.target().name(),
                "telchar_system": config.target().system(),
                "telchar_resource_profile": profile.name(),
            },
        }
    }))
}

fn render_constraint(constraint: &NomadConstraint) -> Value {
    json!({
        "LTarget": constraint.attribute(),
        "Operand": constraint.operator(),
        "RTarget": constraint.value(),
    })
}

fn hmac_capability(
    config: &NomadBackendConfig,
    shared_build_key: &[u8],
    key_id: &str,
    secret_file: &std::path::Path,
    issued_at: SystemTime,
) -> io::Result<String> {
    let secret = fs::read(secret_file)
        .map_err(|_| io::Error::other("Nomad transfer HMAC secret could not be read"))?;
    let issued_at = issued_at
        .duration_since(UNIX_EPOCH)
        .map_err(|_| io::Error::other("Nomad transfer HMAC clock is invalid"))?
        .as_secs();
    let expires_at = issued_at
        .checked_add(config.transfer_limits().authentication_lifetime().as_secs())
        .ok_or_else(|| io::Error::other("Nomad transfer HMAC lifetime is invalid"))?;
    let shared_build_digest = Sha256::digest(shared_build_key);
    let nonce = Sha256::digest(
        [
            shared_build_key,
            config.target().name().as_bytes(),
            config.namespace().as_bytes(),
            &issued_at.to_be_bytes(),
        ]
        .concat(),
    );
    let request_key = Sha256::digest([secret.as_slice(), shared_build_key, &nonce].concat());
    let claims = json!({
        "version": 1,
        "key_id": key_id,
        "backend": config.target().name(),
        "namespace": config.namespace(),
        "job_id": deterministic_job_name(config, shared_build_key),
        "shared_build_digest": URL_SAFE_NO_PAD.encode(shared_build_digest),
        "issued_at": issued_at,
        "expires_at": expires_at,
        "nonce": URL_SAFE_NO_PAD.encode(nonce),
        "request_key": URL_SAFE_NO_PAD.encode(request_key),
    });
    let encoded_claims = URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&claims).map_err(|_| {
            io::Error::other("Nomad transfer HMAC capability could not be encoded")
        })?);
    let mut signer = Hmac::<Sha256>::new_from_slice(&secret)
        .map_err(|_| io::Error::other("Nomad transfer HMAC secret is invalid"))?;
    signer.update(encoded_claims.as_bytes());
    let signature = signer.finalize().into_bytes();
    Ok(format!(
        "{encoded_claims}.{}",
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

fn duration_nanoseconds(duration: std::time::Duration) -> u64 {
    duration
        .as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(duration.subsec_nanos()))
}
