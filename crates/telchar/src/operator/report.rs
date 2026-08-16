//! Defines stable JSON reports assembled from configuration and durable read APIs.

use serde::Serialize;
use telchar::backend::{BackendTarget, ExecutionRecovery};
use telchar::persistence::{
    SharedBuild, SharedBuildOperationalCounts, SharedBuildQueueEntry, SharedBuildState,
};
use telchar::service::config::ServiceConfig;

#[derive(Serialize)]
pub(super) struct ConfigReport {
    valid: bool,
    backend_count: usize,
    backends: Vec<ConfiguredBackend>,
}

impl ConfigReport {
    pub(super) fn from_config(config: &ServiceConfig) -> serde_json::Value {
        let backends = configured_backends(config);
        json(Self {
            valid: true,
            backend_count: backends.len(),
            backends,
        })
    }
}

#[derive(Serialize)]
pub(super) struct StatusReport {
    queued: u64,
    running: u64,
    collecting: u64,
    active: usize,
}

impl StatusReport {
    pub(super) fn read(
        database_url: &str,
    ) -> Result<serde_json::Value, telchar::persistence::SharedBuildError> {
        let SharedBuildOperationalCounts {
            queued,
            running,
            collecting,
        } = telchar::persistence::read_shared_build_operational_counts(database_url)?;
        let active = telchar::persistence::read_active_shared_builds(database_url, 256)?.len();
        Ok(json(Self {
            queued,
            running,
            collecting,
            active,
        }))
    }
}

#[derive(Serialize)]
pub(super) struct QueueReport {
    builds: Vec<QueuedBuild>,
}

impl QueueReport {
    pub(super) fn read(
        database_url: &str,
        limit: usize,
    ) -> Result<serde_json::Value, telchar::persistence::SharedBuildError> {
        let builds = telchar::persistence::read_queued_shared_builds(database_url, limit)?
            .into_iter()
            .map(QueuedBuild::from)
            .collect();
        Ok(json(Self { builds }))
    }
}

#[derive(Serialize)]
struct QueuedBuild {
    derivation_path: String,
    quota_subject: String,
    queue_position: i64,
    queued_at_unix_seconds: u64,
}

impl From<SharedBuildQueueEntry> for QueuedBuild {
    fn from(entry: SharedBuildQueueEntry) -> Self {
        Self {
            derivation_path: entry.derivation_path,
            quota_subject: entry.quota_subject,
            queue_position: entry.queue_position,
            queued_at_unix_seconds: unix_seconds(entry.queued_at),
        }
    }
}

#[derive(Serialize)]
pub(super) struct BuildReport {
    derivation_path: String,
    state: &'static str,
    backend_name: String,
    backend_kind: &'static str,
    recovery: &'static str,
    backend_execution_id: Option<String>,
    expected_outputs: Vec<String>,
    failure_classification: Option<String>,
    attempt: Option<AttemptReport>,
}

impl BuildReport {
    pub(super) fn read(
        database_url: &str,
        derivation_path: &str,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        let build = telchar::persistence::read_shared_build(database_url, derivation_path)?
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "shared build not found")
            })?;
        let attempt =
            telchar::persistence::read_shared_build_attempt(database_url, derivation_path)?
                .map(AttemptReport::from);
        Ok(json(Self {
            derivation_path: build.derivation_path,
            state: state_name(build.state),
            backend_name: build.backend_name,
            backend_kind: build.backend_kind.as_str(),
            recovery: recovery_name(build.capabilities.execution_recovery()),
            backend_execution_id: build.backend_execution_id,
            expected_outputs: build.expected_outputs,
            failure_classification: build.failure_classification,
            attempt,
        }))
    }
}

#[derive(Serialize)]
struct AttemptReport {
    ordinal: i32,
    state: &'static str,
}

impl From<telchar::persistence::SharedBuildAttempt> for AttemptReport {
    fn from(attempt: telchar::persistence::SharedBuildAttempt) -> Self {
        use telchar::persistence::SharedBuildAttemptState;
        Self {
            ordinal: attempt.ordinal,
            state: match attempt.state {
                SharedBuildAttemptState::Running => "running",
                SharedBuildAttemptState::Collecting => "collecting",
                SharedBuildAttemptState::Succeeded => "succeeded",
                SharedBuildAttemptState::Failed => "failed",
            },
        }
    }
}

#[derive(Serialize)]
pub(super) struct BackendReport {
    backends: Vec<BackendState>,
}

impl BackendReport {
    pub(super) fn read(
        config: &ServiceConfig,
        database_url: &str,
    ) -> Result<serde_json::Value, telchar::persistence::SharedBuildError> {
        let active = telchar::persistence::read_active_shared_builds(database_url, 256)?;
        let backends = configured_backends(config)
            .into_iter()
            .map(|backend| BackendState {
                active_builds: active
                    .iter()
                    .filter(|build| build.backend_name == backend.name)
                    .count(),
                backend,
            })
            .collect();
        Ok(json(Self { backends }))
    }
}

#[derive(Serialize)]
struct BackendState {
    #[serde(flatten)]
    backend: ConfiguredBackend,
    active_builds: usize,
}

#[derive(Serialize)]
struct ConfiguredBackend {
    name: String,
    kind: &'static str,
    system: String,
    features: Vec<String>,
    capacity: usize,
}

fn configured_backends(config: &ServiceConfig) -> Vec<ConfiguredBackend> {
    let mut backends = Vec::new();
    if let Some(backend) = config.local_backend() {
        backends.push(configured_backend(
            backend.target(),
            backend.maximum_concurrent_builds(),
        ));
    }
    backends.extend(
        config.static_ssh_backends().iter().map(|backend| {
            configured_backend(backend.target(), backend.maximum_concurrent_builds())
        }),
    );
    backends.extend(
        config.nomad_backends().iter().map(|backend| {
            configured_backend(backend.target(), backend.maximum_concurrent_builds())
        }),
    );
    backends
}

fn configured_backend(target: &BackendTarget, capacity: usize) -> ConfiguredBackend {
    ConfiguredBackend {
        name: target.name().to_owned(),
        kind: target.kind().as_str(),
        system: target.system().to_owned(),
        features: target.features().to_vec(),
        capacity,
    }
}

#[derive(Serialize)]
pub(super) struct RecoveryReport {
    builds: Vec<RecoverableBuild>,
}

impl RecoveryReport {
    pub(super) fn read(
        database_url: &str,
        limit: usize,
    ) -> Result<serde_json::Value, telchar::persistence::SharedBuildError> {
        let builds = telchar::persistence::read_active_shared_builds(database_url, limit)?
            .into_iter()
            .map(RecoverableBuild::from)
            .collect();
        Ok(json(Self { builds }))
    }
}

#[derive(Serialize)]
struct RecoverableBuild {
    derivation_path: String,
    state: &'static str,
    backend_name: String,
    backend_kind: &'static str,
    recovery: &'static str,
    backend_execution_id: Option<String>,
    expected_outputs: usize,
}

impl From<SharedBuild> for RecoverableBuild {
    fn from(build: SharedBuild) -> Self {
        Self {
            derivation_path: build.derivation_path,
            state: state_name(build.state),
            backend_name: build.backend_name,
            backend_kind: build.backend_kind.as_str(),
            recovery: recovery_name(build.capabilities.execution_recovery()),
            backend_execution_id: build.backend_execution_id,
            expected_outputs: build.expected_outputs.len(),
        }
    }
}

fn recovery_name(recovery: ExecutionRecovery) -> &'static str {
    match recovery {
        ExecutionRecovery::OutputOnly => "output-only",
        ExecutionRecovery::Adoptable => "adoptable",
    }
}

fn state_name(state: SharedBuildState) -> &'static str {
    match state {
        SharedBuildState::Claimed => "claimed",
        SharedBuildState::Running => "running",
        SharedBuildState::Collecting => "collecting",
        SharedBuildState::Succeeded => "succeeded",
        SharedBuildState::Failed => "failed",
    }
}

fn unix_seconds(time: std::time::SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn json(value: impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("operator reports contain serializable values")
}
