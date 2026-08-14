//! Reconciles nonterminal shared builds from gateway outputs or the exact persisted backend execution.

use std::io;
use std::time::Duration;

use crate::backend::{BackendCapabilities, BackendKind, ExecutionRecovery};
use crate::persistence::{self, SharedBuild, SharedBuildState};
use crate::store_daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

const MAXIMUM_ACTIVE_SHARED_BUILDS: usize = 256;

pub trait SharedBuildOutputStore {
    fn contains_all(&mut self, outputs: &[String]) -> io::Result<bool>;
}

pub struct GatewaySharedBuildOutputStore {
    endpoint: GatewayStoreEndpoint,
}

impl GatewaySharedBuildOutputStore {
    pub fn from_environment() -> io::Result<Self> {
        let endpoint = std::env::var_os("TELCHAR_GATEWAY_STORE_URI").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "gateway store endpoint is not configured",
            )
        })?;
        let endpoint = GatewayStoreEndpoint::parse_os(&endpoint)?;
        Ok(Self { endpoint })
    }
}

impl SharedBuildOutputStore for GatewaySharedBuildOutputStore {
    fn contains_all(&mut self, outputs: &[String]) -> io::Result<bool> {
        if outputs.is_empty() {
            return Ok(true);
        }
        let mut connection = GatewayStoreConnection::connect(&self.endpoint)?;
        for output in outputs {
            if connection.query_path_info(output.as_bytes())?.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdoptedExecution {
    Monitoring,
    Succeeded,
    Failed,
    Missing,
}

pub trait RecoveryBackend {
    fn capabilities(&self, backend_name: &str) -> Option<(BackendKind, BackendCapabilities)>;

    fn recover_outputs(&mut self, _build: &SharedBuild) -> io::Result<bool> {
        Ok(false)
    }

    fn adopt(&mut self, build: &SharedBuild) -> io::Result<AdoptedExecution>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReconciliationOutcome {
    pub succeeded: usize,
    pub failed: usize,
    pub monitoring: usize,
    pub monitoring_derivations: Vec<String>,
}

pub fn reconcile_active_shared_builds(
    database_url: &str,
    retention: Duration,
    outputs: &mut dyn SharedBuildOutputStore,
    backends: &mut dyn RecoveryBackend,
) -> io::Result<ReconciliationOutcome> {
    let active = persistence::read_active_shared_builds(database_url, MAXIMUM_ACTIVE_SHARED_BUILDS)
        .map_err(|_| io::Error::other("shared build recovery failed"))?;
    reconcile_shared_builds(database_url, retention, active, outputs, backends)
}

pub fn reconcile_adopted_shared_builds(
    database_url: &str,
    retention: Duration,
    derivation_paths: &[String],
    outputs: &mut dyn SharedBuildOutputStore,
    backends: &mut dyn RecoveryBackend,
) -> io::Result<ReconciliationOutcome> {
    let mut active = Vec::with_capacity(derivation_paths.len());
    for derivation_path in derivation_paths {
        let build = persistence::read_shared_build(database_url, derivation_path)
            .map_err(|_| io::Error::other("shared build recovery failed"))?
            .ok_or_else(|| io::Error::other("shared build recovery failed"))?;
        if matches!(
            build.state,
            SharedBuildState::Claimed | SharedBuildState::Running | SharedBuildState::Collecting
        ) {
            active.push(build);
        }
    }
    reconcile_shared_builds(database_url, retention, active, outputs, backends)
}

pub fn reconcile_shared_builds(
    database_url: &str,
    retention: Duration,
    active: Vec<SharedBuild>,
    outputs: &mut dyn SharedBuildOutputStore,
    backends: &mut dyn RecoveryBackend,
) -> io::Result<ReconciliationOutcome> {
    let mut outcome = ReconciliationOutcome::default();
    for build in active {
        if outputs.contains_all(&build.expected_outputs)? {
            complete_recovered_success(database_url, &build, retention)?;
            outcome.succeeded += 1;
            continue;
        }
        let attempt = persistence::read_shared_build_attempt(database_url, &build.derivation_path)
            .map_err(|_| io::Error::other("shared build recovery failed"))?;
        if attempt.as_ref().is_none_or(|attempt| {
            attempt.backend_name != build.backend_name
                || attempt.backend_kind != build.backend_kind
                || attempt.backend_execution_id != build.backend_execution_id
        }) {
            complete_recovery_failure(database_url, &build, retention)?;
            outcome.failed += 1;
            continue;
        }
        let configured = backends.capabilities(&build.backend_name);
        if configured != Some((build.backend_kind, build.capabilities)) {
            complete_recovery_failure(database_url, &build, retention)?;
            outcome.failed += 1;
            continue;
        }
        match build.capabilities.execution_recovery() {
            ExecutionRecovery::OutputOnly => {
                let recovered = match backends.recover_outputs(&build) {
                    Ok(recovered) => recovered,
                    Err(error) => {
                        tracing::warn!(
                            event = "database.shared_build.output_recovery_failed",
                            backend_name = build.backend_name,
                            reason = ?error.kind(),
                            "shared build output recovery failed"
                        );
                        false
                    }
                };
                if recovered {
                    complete_recovered_success(database_url, &build, retention)?;
                    outcome.succeeded += 1;
                } else {
                    complete_recovery_failure(database_url, &build, retention)?;
                    outcome.failed += 1;
                }
            }
            ExecutionRecovery::Adoptable => {
                if build.backend_execution_id.is_none() {
                    complete_recovery_failure(database_url, &build, retention)?;
                    outcome.failed += 1;
                    continue;
                }
                match backends.adopt(&build) {
                    Ok(AdoptedExecution::Monitoring) => {
                        outcome.monitoring += 1;
                        outcome
                            .monitoring_derivations
                            .push(build.derivation_path.clone());
                    }
                    Ok(AdoptedExecution::Succeeded) => {
                        if outputs.contains_all(&build.expected_outputs)? {
                            complete_recovered_success(database_url, &build, retention)?;
                            outcome.succeeded += 1;
                        } else {
                            complete_recovery_failure(database_url, &build, retention)?;
                            outcome.failed += 1;
                        }
                    }
                    Ok(AdoptedExecution::Failed | AdoptedExecution::Missing) | Err(_) => {
                        complete_recovery_failure(database_url, &build, retention)?;
                        outcome.failed += 1;
                    }
                }
            }
        }
    }
    Ok(outcome)
}

fn complete_recovered_success(
    database_url: &str,
    build: &SharedBuild,
    retention: Duration,
) -> io::Result<()> {
    advance_to_collecting(database_url, build)?;
    persistence::complete_shared_build_success(
        database_url,
        &build.derivation_path,
        &serde_json::json!({
            "outputs": build.expected_outputs,
            "recovered": true,
        }),
        retention,
    )
    .map_err(|_| io::Error::other("shared build recovery failed"))?;
    Ok(())
}

fn advance_to_collecting(database_url: &str, build: &SharedBuild) -> io::Result<()> {
    match build.state {
        SharedBuildState::Claimed => {
            persistence::start_shared_build(database_url, &build.derivation_path)
                .map_err(|_| io::Error::other("shared build recovery failed"))?;
            persistence::collect_shared_build(database_url, &build.derivation_path)
                .map_err(|_| io::Error::other("shared build recovery failed"))?;
        }
        SharedBuildState::Running => {
            persistence::collect_shared_build(database_url, &build.derivation_path)
                .map_err(|_| io::Error::other("shared build recovery failed"))?;
        }
        SharedBuildState::Collecting => {}
        SharedBuildState::Succeeded | SharedBuildState::Failed => {
            return Err(io::Error::other("shared build recovery failed"));
        }
    }
    Ok(())
}

fn complete_recovery_failure(
    database_url: &str,
    build: &SharedBuild,
    retention: Duration,
) -> io::Result<()> {
    persistence::complete_shared_build_failure(
        database_url,
        &build.derivation_path,
        "restart-recovery-failed",
        &serde_json::json!({"stage": "restart-recovery"}),
        retention,
    )
    .map_err(|_| io::Error::other("shared build recovery failed"))?;
    Ok(())
}
