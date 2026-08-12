use std::fs;
use std::io::{self, Read};
use std::time::Instant;

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Certificate, Identity};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::backend::{BuildExecution, BuildResult, BuildStatus, OutputTrust};
use crate::config::{NomadBackendConfig, NomadTransferAuthentication};

const MAXIMUM_NOMAD_RESPONSE_BYTES: u64 = 1024 * 1024;

pub struct NomadClient {
    config: NomadBackendConfig,
    client: Client,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NomadExecutionState {
    Monitoring,
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
    #[serde(rename = "Meta")]
    meta: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct AllocationResponse {
    #[serde(rename = "ClientStatus")]
    client_status: String,
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

    pub fn status(&self, job_id: &str) -> io::Result<NomadExecutionState> {
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
        if allocations.is_empty() {
            return Ok(NomadExecutionState::Monitoring);
        }
        if allocations
            .iter()
            .any(|allocation| allocation.client_status == "failed")
        {
            return Ok(NomadExecutionState::Failed);
        }
        if allocations
            .iter()
            .all(|allocation| allocation.client_status == "complete")
        {
            return Ok(NomadExecutionState::Succeeded);
        }
        Ok(NomadExecutionState::Monitoring)
    }

    pub fn execute(
        &self,
        execution: &BuildExecution<'_>,
        shared_build_key: &[u8],
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        let submission = self.submit(shared_build_key)?;
        let started = Instant::now();
        loop {
            if cancelled()? {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Nomad job execution cancelled",
                ));
            }
            match self.status(submission.job_id())? {
                NomadExecutionState::Monitoring => {}
                NomadExecutionState::Succeeded => {
                    return BuildResult::new(
                        BuildStatus::Built,
                        execution.build().expected_outputs().to_vec(),
                        OutputTrust::TrustedExecutor,
                    );
                }
                NomadExecutionState::Failed | NomadExecutionState::Missing => {
                    return Err(io::Error::other("Nomad job execution failed"));
                }
            }
            if started.elapsed() >= execution.timeout() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Nomad job execution timed out",
                ));
            }
            std::thread::sleep(self.config.poll_interval());
        }
    }

    pub fn submit(&self, shared_build_key: &[u8]) -> io::Result<NomadSubmission> {
        let job_id = deterministic_job_name(&self.config, shared_build_key);
        let response = self
            .client
            .post(format!("{}/v1/jobs", self.config.endpoint()))
            .query(&[("namespace", self.config.namespace())])
            .json(&render_job(&self.config, shared_build_key))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|_| io::Error::other("Nomad job submission failed"))?;
        let parsed: SubmissionResponse = bounded_json(response, "Nomad job submission failed")?;
        if parsed.eval_id.is_empty() || parsed.eval_id.len() > 256 {
            return Err(io::Error::other("Nomad job submission failed"));
        }
        Ok(NomadSubmission {
            job_id,
            evaluation_id: parsed.eval_id,
        })
    }
}

fn bounded_json<T: serde::de::DeserializeOwned>(
    response: reqwest::blocking::Response,
    failure: &'static str,
) -> io::Result<T> {
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
    serde_json::from_slice(&bytes).map_err(|_| io::Error::other(failure))
}

pub fn deterministic_job_name(config: &NomadBackendConfig, shared_build_key: &[u8]) -> String {
    let digest = Sha256::digest(shared_build_key);
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}-{suffix}", config.job_name_scope())
}

pub fn render_job(config: &NomadBackendConfig, shared_build_key: &[u8]) -> Value {
    let mut task = json!({
        "Name": "build",
        "Driver": config.driver(),
        "Config": Value::Object(config.driver_config().clone()),
        "Resources": {
            "CPU": config.resources().cpu_mhz(),
            "MemoryMB": config.resources().memory_mb(),
            "DiskMB": config.resources().disk_mb(),
        },
        "Env": {
            "TELCHAR_TRANSFER_ENDPOINT": config.transfer_endpoint(),
            "TELCHAR_NIX_STORE_URI": config.store().uri(),
        },
    });
    if let NomadTransferAuthentication::WorkloadIdentity { .. } = config.transfer_authentication() {
        task["Identity"] = json!({
            "Env": true,
            "File": false,
            "Audiences": [config
                .transfer_authentication()
                .audience()
                .expect("workload identity audience is configured")],
        });
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
    group.insert("Tasks".to_owned(), Value::Array(tasks));
    json!({
        "Job": {
            "ID": deterministic_job_name(config, shared_build_key),
            "Name": deterministic_job_name(config, shared_build_key),
            "Type": "batch",
            "Namespace": config.namespace(),
            "Datacenters": ["*"],
            "TaskGroups": [Value::Object(group)],
            "Meta": {
                "telchar_backend": config.target().name(),
                "telchar_system": config.target().system(),
            },
        }
    })
}

fn duration_nanoseconds(duration: std::time::Duration) -> u64 {
    duration
        .as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(duration.subsec_nanos()))
}
