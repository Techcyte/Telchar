//! Reconciles nonterminal shared builds from gateway outputs or the exact persisted backend execution.

use std::io;
use std::time::Duration;

use crate::backend::{BackendCapabilities, BackendKind, ExecutionRecovery};
use crate::persistence::{self, SharedBuild, SharedBuildState};
use crate::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

const MAXIMUM_ACTIVE_SHARED_BUILDS: usize = 256;

pub trait SharedBuildOutputStore {
    fn contains_all(&mut self, outputs: &[String]) -> io::Result<bool>;

    fn validate_outputs(&mut self, request: &crate::build::BuildRequest) -> io::Result<()>;

    fn retain_outputs(
        &mut self,
        database: &dyn crate::persistence::DatabaseSource,
        build: &SharedBuild,
        retention: Duration,
    ) -> io::Result<()>;

    fn protect_hook_delivery(
        &mut self,
        database: &dyn crate::persistence::DatabaseSource,
        build: &SharedBuild,
    ) -> io::Result<()>;
}

pub struct GatewaySharedBuildOutputStore {
    endpoint: Option<GatewayStoreEndpoint>,
    hook_delivery_retention: Option<Box<dyn crate::store::retention::StoreRetentionBackend>>,
    output_protection: Option<GatewayStoreConnection>,
}

impl GatewaySharedBuildOutputStore {
    pub fn new(endpoint: GatewayStoreEndpoint) -> Self {
        Self {
            endpoint: Some(endpoint),
            hook_delivery_retention: None,
            output_protection: None,
        }
    }

    pub fn with_endpoint(endpoint: Option<GatewayStoreEndpoint>) -> Self {
        Self {
            endpoint,
            hook_delivery_retention: None,
            output_protection: None,
        }
    }

    pub fn hook_delivery_retention(
        mut self,
        retention: Box<dyn crate::store::retention::StoreRetentionBackend>,
    ) -> Self {
        self.hook_delivery_retention = Some(retention);
        self
    }

    pub fn from_environment() -> io::Result<Self> {
        let endpoint = std::env::var_os("TELCHAR_GATEWAY_STORE_URI").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "gateway store endpoint is not configured",
            )
        })?;
        let endpoint = GatewayStoreEndpoint::parse_os(&endpoint)?;
        Ok(Self::new(endpoint))
    }
}

impl SharedBuildOutputStore for GatewaySharedBuildOutputStore {
    fn retain_outputs(
        &mut self,
        database: &dyn crate::persistence::DatabaseSource,
        build: &SharedBuild,
        duration: Duration,
    ) -> io::Result<()> {
        let attempt = persistence::read_shared_build_attempt(database, &build.derivation_path)
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("recovered attempt is missing"))?;
        if attempt.state != persistence::SharedBuildAttemptState::Collecting {
            return Err(io::Error::other(
                "output retention requires collecting attempt",
            ));
        }
        if attempt.backend_name != build.backend_name
            || attempt.backend_kind != build.backend_kind
            || attempt.backend_execution_id != build.backend_execution_id
        {
            return Err(io::Error::other(
                "output retention execution identity mismatch",
            ));
        }
        let request = build
            .build_request
            .as_ref()
            .ok_or_else(|| io::Error::other("recovered specification is missing"))?;
        let request_id = format!("recovery-{}", attempt.attempt_id);
        if persistence::read_build_request(database, &request_id)
            .map_err(io::Error::other)?
            .is_none()
        {
            persistence::create_build_request(
                database,
                &request_id,
                &build.derivation_path,
                request.system(),
                "recovery",
                "recovery",
            )
            .map_err(io::Error::other)?;
        }
        let leases = build
            .expected_outputs
            .iter()
            .enumerate()
            .map(|(index, path)| {
                (
                    format!("recovery-{}-{index}", attempt.attempt_id),
                    path.clone(),
                )
            })
            .collect::<Vec<_>>();
        let retention = self
            .hook_delivery_retention
            .as_mut()
            .ok_or_else(|| io::Error::other("recovered output retention is unavailable"))?;
        let entries = leases
            .iter()
            .map(|(id, path)| crate::store::retention::RetentionEntry::new(id, path))
            .collect::<Vec<_>>();
        let mut present = 0;
        for (id, path) in &leases {
            if let Some(lease) =
                persistence::read_store_lease(database, id).map_err(io::Error::other)?
            {
                if lease.store_path != *path
                    || lease.owner_id != request_id
                    || lease.purpose != persistence::StoreLeasePurpose::Output
                    || lease.state != persistence::StoreLeaseState::Active
                    || lease
                        .expires_at
                        .is_none_or(|expires| expires <= std::time::SystemTime::now())
                {
                    return Err(io::Error::other("recovered output lease is inconsistent"));
                }
                present += 1;
            }
        }
        if present == 0 {
            persistence::create_request_output_leases(database, &request_id, duration, &leases)
                .map_err(io::Error::other)?;
        } else if present != leases.len() {
            return Err(io::Error::other("recovered output lease set is incomplete"));
        }
        retention.retain(&entries)?;
        Ok(())
    }

    fn validate_outputs(&mut self, request: &crate::build::BuildRequest) -> io::Result<()> {
        let endpoint = self
            .endpoint
            .as_ref()
            .ok_or_else(|| io::Error::other("gateway store endpoint is not configured"))?;
        if self.output_protection.is_none() {
            self.output_protection = Some(GatewayStoreConnection::connect(endpoint)?);
        }
        let protection = self
            .output_protection
            .as_mut()
            .expect("output protection connected");
        for (_, path) in request.expected_outputs() {
            protection.add_temporary_root(path)?;
        }
        let mut export = crate::store::export::GatewayStoreExportBackend::new(endpoint.clone());
        for authority in request.output_authorities() {
            let path = std::str::from_utf8(authority.path())
                .map_err(|_| io::Error::other("recovered output path is invalid"))?;
            crate::store::export::validate_store_output_content(
                std::path::Path::new(path),
                authority.expected_content_address().as_deref(),
                &mut export,
            )
            .map_err(|error| {
                if error.kind() == io::ErrorKind::InvalidData
                    && error.to_string() == "build output content address mismatch"
                {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "recovered output content address mismatch",
                    )
                } else {
                    error
                }
            })?;
        }
        Ok(())
    }

    fn protect_hook_delivery(
        &mut self,
        database: &dyn crate::persistence::DatabaseSource,
        build: &SharedBuild,
    ) -> io::Result<()> {
        if !database
            .database()
            .map_err(io::Error::other)?
            .post_build_hooks_enabled()
        {
            return Ok(());
        }
        let retention = self
            .hook_delivery_retention
            .as_mut()
            .ok_or_else(|| io::Error::other("hook delivery retention is unavailable"))?;
        let endpoint = self
            .endpoint
            .as_ref()
            .ok_or_else(|| io::Error::other("gateway store endpoint is not configured"))?;
        let mut protection = GatewayStoreConnection::connect(endpoint)?;
        for output in &build.expected_outputs {
            protection.add_temporary_root(output.as_bytes())?;
            if !protection.is_valid_path(output.as_bytes())? {
                return Err(io::Error::other("hook delivery output is missing"));
            }
        }
        crate::service::hook_delivery::prepare_outputs(
            database,
            &build.derivation_path,
            retention.as_mut(),
            &build.expected_outputs,
        )
    }

    fn contains_all(&mut self, outputs: &[String]) -> io::Result<bool> {
        if outputs.is_empty() {
            return Ok(true);
        }
        let endpoint = self.endpoint.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "gateway store endpoint is not configured",
            )
        })?;
        let mut connection = GatewayStoreConnection::connect(endpoint)?;
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
    database: &(impl crate::persistence::DatabaseSource + ?Sized),
    retention: Duration,
    outputs: &mut dyn SharedBuildOutputStore,
    backends: &mut dyn RecoveryBackend,
) -> io::Result<ReconciliationOutcome> {
    let mut cursor = None;
    let mut outcome = ReconciliationOutcome::default();
    loop {
        let active = persistence::read_active_shared_builds_after(
            database,
            cursor
                .as_ref()
                .map(|(created_at, path): &(std::time::SystemTime, String)| {
                    (*created_at, path.as_str())
                }),
            MAXIMUM_ACTIVE_SHARED_BUILDS,
        )
        .map_err(|_| io::Error::other("shared build recovery failed"))?;
        let Some(last) = active.last() else {
            break;
        };
        cursor = Some((last.created_at, last.derivation_path.clone()));
        let page = reconcile_shared_builds(database, retention, active, outputs, backends)?;
        outcome.succeeded += page.succeeded;
        outcome.failed += page.failed;
        outcome.monitoring += page.monitoring;
        outcome
            .monitoring_derivations
            .extend(page.monitoring_derivations);
    }
    Ok(outcome)
}

pub fn reconcile_adopted_shared_builds(
    database: &(impl crate::persistence::DatabaseSource + ?Sized),
    retention: Duration,
    derivation_paths: &[String],
    outputs: &mut dyn SharedBuildOutputStore,
    backends: &mut dyn RecoveryBackend,
) -> io::Result<ReconciliationOutcome> {
    let mut active = Vec::with_capacity(derivation_paths.len());
    for derivation_path in derivation_paths {
        let build = persistence::read_shared_build(database, derivation_path)
            .map_err(|_| io::Error::other("shared build recovery failed"))?
            .ok_or_else(|| io::Error::other("shared build recovery failed"))?;
        if matches!(
            build.state,
            SharedBuildState::Claimed | SharedBuildState::Running | SharedBuildState::Collecting
        ) {
            active.push(build);
        }
    }
    reconcile_shared_builds(database, retention, active, outputs, backends)
}

pub fn reconcile_shared_builds(
    database: &(impl crate::persistence::DatabaseSource + ?Sized),
    retention: Duration,
    active: Vec<SharedBuild>,
    outputs: &mut dyn SharedBuildOutputStore,
    backends: &mut dyn RecoveryBackend,
) -> io::Result<ReconciliationOutcome> {
    let mut outcome = ReconciliationOutcome::default();
    for build in active {
        if outputs.contains_all(&build.expected_outputs)? {
            complete_recovered_success(database, &build, retention, outputs)?;
            outcome.succeeded += 1;
            continue;
        }
        let attempt = persistence::read_shared_build_attempt(database, &build.derivation_path)
            .map_err(|_| io::Error::other("shared build recovery failed"))?;
        if attempt.as_ref().is_none_or(|attempt| {
            attempt.backend_name != build.backend_name
                || attempt.backend_kind != build.backend_kind
                || attempt.backend_execution_id != build.backend_execution_id
        }) {
            complete_recovery_failure(database, &build, retention)?;
            outcome.failed += 1;
            continue;
        }
        let configured = backends.capabilities(&build.backend_name);
        if configured != Some((build.backend_kind, build.capabilities)) {
            complete_recovery_failure(database, &build, retention)?;
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
                    complete_recovered_success(database, &build, retention, outputs)?;
                    outcome.succeeded += 1;
                } else {
                    complete_recovery_failure(database, &build, retention)?;
                    outcome.failed += 1;
                }
            }
            ExecutionRecovery::Adoptable => {
                if build.backend_execution_id.is_none() {
                    complete_recovery_failure(database, &build, retention)?;
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
                            complete_recovered_success(database, &build, retention, outputs)?;
                            outcome.succeeded += 1;
                        } else {
                            complete_recovery_failure(database, &build, retention)?;
                            outcome.failed += 1;
                        }
                    }
                    Ok(AdoptedExecution::Failed | AdoptedExecution::Missing) | Err(_) => {
                        complete_recovery_failure(database, &build, retention)?;
                        outcome.failed += 1;
                    }
                }
            }
        }
    }
    Ok(outcome)
}

fn complete_recovered_success(
    database: &(impl crate::persistence::DatabaseSource + ?Sized),
    build: &SharedBuild,
    retention: Duration,
    outputs: &mut dyn SharedBuildOutputStore,
) -> io::Result<()> {
    let request = build
        .build_request
        .as_ref()
        .ok_or_else(|| io::Error::other("recovered build specification is unavailable"))?;
    let paths = request
        .expected_outputs()
        .iter()
        .map(|(_, path)| std::str::from_utf8(path))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| io::Error::other("recovered output path is invalid"))?;
    if paths != build.expected_outputs {
        return Err(io::Error::other("recovered output identity mismatch"));
    }
    outputs.validate_outputs(request)?;
    advance_to_collecting(database, build)?;
    let retention_database = database.database().map_err(io::Error::other)?;
    outputs.retain_outputs(&retention_database, build, retention)?;
    let hook_delivery_database = database.database().map_err(io::Error::other)?;
    outputs.protect_hook_delivery(&hook_delivery_database, build)?;
    persistence::complete_shared_build_success(
        database,
        &build.derivation_path,
        &serde_json::json!({
            "status": "built",
            "outputs": request.expected_outputs().iter().map(|(name, path)| serde_json::json!({
                "name": String::from_utf8_lossy(name),
                "path": String::from_utf8_lossy(path),
            })).collect::<Vec<_>>(),
            "recovered": true,
        }),
        retention,
    )
    .map_err(|_| io::Error::other("shared build recovery failed"))?;
    Ok(())
}

fn advance_to_collecting(
    database: &(impl crate::persistence::DatabaseSource + ?Sized),
    build: &SharedBuild,
) -> io::Result<()> {
    match build.state {
        SharedBuildState::Claimed => {
            persistence::start_shared_build(database, &build.derivation_path)
                .map_err(|_| io::Error::other("shared build recovery failed"))?;
            persistence::collect_shared_build(database, &build.derivation_path)
                .map_err(|_| io::Error::other("shared build recovery failed"))?;
        }
        SharedBuildState::Running => {
            persistence::collect_shared_build(database, &build.derivation_path)
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
    database: &(impl crate::persistence::DatabaseSource + ?Sized),
    build: &SharedBuild,
    retention: Duration,
) -> io::Result<()> {
    persistence::complete_shared_build_failure(
        database,
        &build.derivation_path,
        "restart-recovery-failed",
        &serde_json::json!({"stage": "restart-recovery"}),
        retention,
    )
    .map_err(|_| io::Error::other("shared build recovery failed"))?;
    Ok(())
}
