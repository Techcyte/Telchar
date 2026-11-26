//! Constructs configured backend executors and dispatches admitted builds to the selected target.

#[cfg(test)]
mod admission_tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn reload_preserves_occupied_objects_and_consumed_rate_tokens() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("service.toml");
        std::fs::write(&path, "[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n").unwrap();
        let mut config = ServiceConfig::load_from_default(&path).unwrap();
        let limits = crate::service::transfer_limits::TransferLimits {
            maximum_active_inbound_objects: 1,
            inbound_rate_bytes_per_second: 1,
            inbound_burst_bytes: 1,
            ..Default::default()
        };
        let admission = crate::service::transfer_limits::ObjectAdmissionState::new(&limits);
        let held = admission.admit_inbound().unwrap();
        let mut budget = crate::service::transfer_limits::TransferBudget::new(1);
        let mut frontend = crate::service::transfer_limits::LimitedReader::with_rate(
            std::io::Cursor::new([1]),
            1,
            &mut budget,
            admission.rates(),
        );
        frontend.read_exact(&mut [0]).unwrap();
        let initial = ConfiguredBackends::new(&config, None)
            .unwrap()
            .with_object_admission(admission.clone());
        let mut health = crate::service::daemon_services::StaticSshHealthService::start(
            initial.static_ssh_health(),
            std::time::Duration::from_secs(60),
        )
        .unwrap();
        let backends = ReloadableBackends::new(initial.clone());
        crate::service::config_reload::BackendReload::prepare_config(
            &config,
            config.clone(),
            None,
            None,
            std::time::Duration::from_secs(60),
        )
        .unwrap()
        .apply(&mut config, &backends, &mut health)
        .unwrap();
        for generation in [initial, backends.snapshot()] {
            assert!(generation.inner.object_admission.admit_inbound().is_err());
            let mut destination = Vec::new();
            let mut backend = crate::service::transfer_limits::RateWriter::new(
                &mut destination,
                generation.inner.object_admission.rates(),
                true,
            );
            assert_eq!(
                backend.write_all(&[2]).unwrap_err().to_string(),
                "transfer rate limit exceeded"
            );
            assert!(destination.is_empty());
        }
        drop(held);
        assert!(
            backends
                .snapshot()
                .inner
                .object_admission
                .admit_inbound()
                .is_ok()
        );
        health.shutdown().unwrap();
    }
}

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::backend::{
    BackendCapabilities, BackendKind, BackendPool, BuildBackend, BuildExecution, BuildResult,
};
use crate::service::config::{NomadBackendConfig, ServiceConfig, StaticSshBackendConfig};
use crate::store::daemon::GatewayStoreEndpoint;

#[derive(Clone)]
pub struct ConfiguredBackends {
    inner: Arc<ConfiguredBackendsInner>,
}

struct ConfiguredBackendsInner {
    pool: BackendPool,
    object_admission: crate::service::transfer_limits::ObjectAdmissionState,
    uncertain_nomad_permits: Arc<std::sync::Mutex<Vec<crate::backend::BackendPermit>>>,
    nomad_obligations:
        Arc<std::sync::Mutex<std::collections::BTreeMap<i64, crate::backend::BackendPermit>>>,
    nomad_cleanup_cursor: Arc<std::sync::Mutex<Option<i64>>>,
    permit_wait: std::time::Duration,
    gateway_store: Option<GatewayStoreEndpoint>,
    local_build_helper: Option<PathBuf>,
    static_ssh: Vec<StaticSshBackendConfig>,
    static_ssh_health: crate::backend::static_ssh::StaticSshHealth,
    schedulable_static_ssh: Arc<RwLock<BTreeSet<String>>>,
    nomad: Vec<NomadBackendConfig>,
}

impl ConfiguredBackends {
    pub fn new(
        config: &ServiceConfig,
        gateway_store: impl Into<Option<GatewayStoreEndpoint>>,
    ) -> io::Result<Self> {
        let health =
            crate::backend::static_ssh::StaticSshHealth::probe_all(config.static_ssh_backends());
        Self::with_health(config, gateway_store, None, health)
    }

    pub fn with_local_build_helper(
        config: &ServiceConfig,
        gateway_store: impl Into<Option<GatewayStoreEndpoint>>,
        local_build_helper: Option<PathBuf>,
    ) -> io::Result<Self> {
        let health =
            crate::backend::static_ssh::StaticSshHealth::probe_all(config.static_ssh_backends());
        Self::with_health(config, gateway_store, local_build_helper, health)
    }

    pub fn with_health(
        config: &ServiceConfig,
        gateway_store: impl Into<Option<GatewayStoreEndpoint>>,
        local_build_helper: Option<PathBuf>,
        static_ssh_health: crate::backend::static_ssh::StaticSshHealth,
    ) -> io::Result<Self> {
        let schedulable_static_ssh = Arc::new(RwLock::new(
            config
                .static_ssh_backends()
                .iter()
                .map(|backend| backend.target().name().to_owned())
                .collect(),
        ));
        Self::with_health_and_scheduling(
            config,
            gateway_store,
            local_build_helper,
            static_ssh_health,
            schedulable_static_ssh,
        )
    }

    pub fn with_health_and_scheduling(
        config: &ServiceConfig,
        gateway_store: impl Into<Option<GatewayStoreEndpoint>>,
        local_build_helper: Option<PathBuf>,
        static_ssh_health: crate::backend::static_ssh::StaticSshHealth,
        schedulable_static_ssh: Arc<RwLock<BTreeSet<String>>>,
    ) -> io::Result<Self> {
        Self::with_static_ssh_inventory_and_health(
            config,
            config.static_ssh_backends().to_vec(),
            gateway_store,
            local_build_helper,
            static_ssh_health,
            schedulable_static_ssh,
        )
    }

    pub fn with_static_ssh_inventory(
        config: &ServiceConfig,
        static_ssh: Vec<StaticSshBackendConfig>,
        gateway_store: impl Into<Option<GatewayStoreEndpoint>>,
        local_build_helper: Option<PathBuf>,
    ) -> io::Result<Self> {
        let health = crate::backend::static_ssh::StaticSshHealth::probe_all(&static_ssh);
        let scheduling = Arc::new(RwLock::new(
            static_ssh
                .iter()
                .map(|backend| backend.target().name().to_owned())
                .collect(),
        ));
        Self::with_static_ssh_inventory_and_health(
            config,
            static_ssh,
            gateway_store,
            local_build_helper,
            health,
            scheduling,
        )
    }

    pub(crate) fn with_static_ssh_inventory_and_health(
        config: &ServiceConfig,
        static_ssh: Vec<StaticSshBackendConfig>,
        gateway_store: impl Into<Option<GatewayStoreEndpoint>>,
        local_build_helper: Option<PathBuf>,
        static_ssh_health: crate::backend::static_ssh::StaticSshHealth,
        schedulable_static_ssh: Arc<RwLock<BTreeSet<String>>>,
    ) -> io::Result<Self> {
        Self::compose(
            config,
            static_ssh,
            gateway_store.into(),
            local_build_helper,
            static_ssh_health,
            schedulable_static_ssh,
        )
    }

    fn compose(
        config: &ServiceConfig,
        static_ssh: Vec<StaticSshBackendConfig>,
        gateway_store: Option<GatewayStoreEndpoint>,
        local_build_helper: Option<PathBuf>,
        static_ssh_health: crate::backend::static_ssh::StaticSshHealth,
        schedulable_static_ssh: Arc<RwLock<BTreeSet<String>>>,
    ) -> io::Result<Self> {
        let mut targets = Vec::new();
        let mut maximums = Vec::new();
        if let Some(local) = config.local_backend() {
            targets.push(local.target().clone());
            maximums.push(local.maximum_concurrent_builds());
        }
        for backend in &static_ssh {
            targets.push(backend.target().clone());
            maximums.push(backend.maximum_concurrent_builds());
        }
        for backend in config.nomad_backends() {
            targets.push(backend.target().clone());
            maximums.push(backend.maximum_concurrent_builds());
        }
        Ok(Self {
            inner: Arc::new(ConfiguredBackendsInner {
                pool: BackendPool::new(targets, maximums)?,
                object_admission: crate::service::transfer_limits::ObjectAdmissionState::new(
                    &crate::service::transfer_limits::TransferLimits::from_environment()?,
                ),
                uncertain_nomad_permits: Arc::new(std::sync::Mutex::new(Vec::new())),
                nomad_obligations: Arc::new(std::sync::Mutex::new(
                    std::collections::BTreeMap::new(),
                )),
                nomad_cleanup_cursor: Arc::new(std::sync::Mutex::new(None)),
                permit_wait: config.backend_permit_wait(),
                gateway_store,
                local_build_helper,
                static_ssh,
                static_ssh_health,
                schedulable_static_ssh,
                nomad: config.nomad_backends().to_vec(),
            }),
        })
    }

    pub fn with_object_admission(
        mut self,
        admission: crate::service::transfer_limits::ObjectAdmissionState,
    ) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("exclusive routing construction")
            .object_admission = admission;
        self
    }

    pub(crate) fn retain_runtime_state(
        self,
        config: &ServiceConfig,
        current: &Self,
    ) -> io::Result<Self> {
        let mut replacement = Self::compose(
            config,
            self.inner.static_ssh.clone(),
            self.inner.gateway_store.clone(),
            self.inner.local_build_helper.clone(),
            self.inner.static_ssh_health.clone(),
            Arc::clone(&self.inner.schedulable_static_ssh),
        )?;
        let inner = Arc::get_mut(&mut replacement.inner).expect("exclusive routing construction");
        inner.pool = inner.pool.clone().retain_capacity(&current.inner.pool)?;
        inner.object_admission = current.inner.object_admission.clone();
        inner.nomad_obligations = Arc::clone(&current.inner.nomad_obligations);
        inner.nomad_cleanup_cursor = Arc::clone(&current.inner.nomad_cleanup_cursor);
        inner.uncertain_nomad_permits = Arc::clone(&current.inner.uncertain_nomad_permits);
        Ok(replacement)
    }

    pub fn targets(&self) -> impl Iterator<Item = &crate::backend::BackendTarget> {
        self.inner.pool.targets()
    }

    pub fn static_ssh_health(&self) -> crate::backend::static_ssh::StaticSshHealth {
        self.inner.static_ssh_health.clone()
    }

    pub fn static_ssh_scheduling(&self) -> Arc<RwLock<BTreeSet<String>>> {
        Arc::clone(&self.inner.schedulable_static_ssh)
    }

    pub fn reconcile_nomad_capacity(
        &self,
        database: &crate::persistence::Database,
    ) -> io::Result<()> {
        let mut cursor = self
            .inner
            .nomad_cleanup_cursor
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(budget) = self
            .inner
            .nomad
            .iter()
            .map(|config| config.cleanup_timeout())
            .min()
        else {
            return Ok(());
        };
        let deadline = std::time::Instant::now() + budget;
        let mut attempts = self
            .inner
            .nomad_obligations
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .copied()
            .collect::<Vec<_>>();
        if let Some(after) = *cursor {
            let start = attempts.partition_point(|id| *id <= after);
            attempts.rotate_left(start);
        }
        for attempt_id in attempts {
            if std::time::Instant::now() >= deadline {
                break;
            }
            *cursor = Some(attempt_id);
            let attempt = crate::persistence::read_shared_build_attempt_by_id(database, attempt_id)
                .map_err(|error| io::Error::other(error.to_string()))?
                .ok_or_else(|| io::Error::other("obligated Nomad attempt is missing"))?;
            let backend = attempt.backend_name;
            let job = attempt
                .backend_execution_id
                .ok_or_else(|| io::Error::other("obligated Nomad execution identity is missing"))?;
            let config = self
                .inner
                .nomad
                .iter()
                .find(|config| config.target().name() == backend)
                .ok_or_else(|| io::Error::other("obligated Nomad backend is not configured"))?;
            let cleanup = crate::nomad::backend::NomadClient::new(config.clone())?
                .execution_cleanup_before(&job, deadline);
            let complete = match cleanup {
                Ok(complete) => complete,
                Err(error) if crate::nomad::diagnostics::transient_cleanup_failure(&error) => {
                    tracing::warn!(
                        event = "nomad.cleanup.deferred",
                        backend_name = backend,
                        attempt_id,
                        "Nomad cleanup deferred while execution liability remains retained"
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            if complete {
                crate::persistence::resolve_nomad_execution_obligation(database, attempt_id)
                    .map_err(|error| io::Error::other(error.to_string()))?;
                self.inner
                    .nomad_obligations
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&attempt_id);
            }
        }
        Ok(())
    }

    pub fn restore_nomad_capacity(
        &self,
        database: &crate::persistence::Database,
    ) -> io::Result<()> {
        let pending = crate::persistence::read_nomad_execution_obligations(database)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut retained = self
            .inner
            .nomad_obligations
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        retained.retain(|attempt_id, _| pending.iter().any(|(id, _)| id == attempt_id));
        for (attempt_id, backend) in pending {
            if let std::collections::btree_map::Entry::Vacant(entry) = retained.entry(attempt_id) {
                entry.insert(self.inner.pool.restore_target(&backend)?);
            }
        }
        Ok(())
    }

    pub fn executor(
        &self,
        database: crate::persistence::Database,
        shared_builds: Arc<crate::shared_build::SharedBuildRegistry>,
    ) -> io::Result<BackendExecutor> {
        Ok(BackendExecutor {
            backends: self.clone(),
            database,
            shared_builds,
        })
    }
}

#[derive(Clone)]
pub struct ReloadableBackends {
    current: Arc<RwLock<ConfiguredBackends>>,
}

impl ReloadableBackends {
    pub fn new(backends: ConfiguredBackends) -> Self {
        Self {
            current: Arc::new(RwLock::new(backends)),
        }
    }

    pub fn snapshot(&self) -> ConfiguredBackends {
        self.current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn disable_static_ssh_not_in(&self, desired: &BTreeSet<String>) {
        let scheduling = self.snapshot().static_ssh_scheduling();
        scheduling
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|name| desired.contains(name));
    }

    pub fn replace(&self, backends: ConfiguredBackends) {
        *self
            .current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = backends;
    }
}

impl crate::shared_build::recovery::RecoveryBackend for ConfiguredBackends {
    fn capabilities(&self, backend_name: &str) -> Option<(BackendKind, BackendCapabilities)> {
        self.inner
            .pool
            .targets()
            .find(|target| target.name() == backend_name)
            .map(|target| (target.kind(), target.capabilities()))
    }

    fn recover_outputs(&mut self, build: &crate::persistence::SharedBuild) -> io::Result<bool> {
        let config = self
            .inner
            .static_ssh
            .iter()
            .find(|config| config.target().name() == build.backend_name)
            .ok_or_else(|| io::Error::other("static SSH backend is not configured"))?;
        let gateway_store = self
            .inner
            .gateway_store
            .as_ref()
            .ok_or_else(|| io::Error::other("gateway store endpoint is not configured"))?;
        crate::backend::static_ssh::recover_outputs(
            config,
            gateway_store,
            &build.expected_outputs,
            std::time::Duration::from_secs(10),
            self.inner.object_admission.clone(),
        )?;
        Ok(true)
    }

    fn adopt(
        &mut self,
        build: &crate::persistence::SharedBuild,
    ) -> io::Result<crate::shared_build::recovery::AdoptedExecution> {
        let config = self
            .inner
            .nomad
            .iter()
            .find(|config| config.target().name() == build.backend_name)
            .ok_or_else(|| io::Error::other("Nomad backend is not configured"))?;
        let execution_id = build
            .backend_execution_id
            .as_deref()
            .ok_or_else(|| io::Error::other("Nomad execution identity is unavailable"))?;
        match crate::nomad::backend::NomadClient::new(config.clone())?.status(execution_id)? {
            crate::nomad::backend::NomadExecutionState::Pending
            | crate::nomad::backend::NomadExecutionState::Placed => {
                Ok(crate::shared_build::recovery::AdoptedExecution::Monitoring)
            }
            crate::nomad::backend::NomadExecutionState::Succeeded => {
                Ok(crate::shared_build::recovery::AdoptedExecution::Succeeded)
            }
            crate::nomad::backend::NomadExecutionState::Failed => {
                Ok(crate::shared_build::recovery::AdoptedExecution::Failed)
            }
            crate::nomad::backend::NomadExecutionState::Missing => {
                Ok(crate::shared_build::recovery::AdoptedExecution::Missing)
            }
        }
    }
}

pub struct BackendExecutor {
    backends: ConfiguredBackends,
    database: crate::persistence::Database,
    shared_builds: Arc<crate::shared_build::SharedBuildRegistry>,
}

impl BackendExecutor {
    fn reserve_with_producers(
        &self,
        system: &str,
        required_features: &[&str],
        producers: &[crate::persistence::OutputProducer],
    ) -> io::Result<Option<crate::backend::BackendPermit>> {
        self.backends
            .inner
            .pool
            .acquire_ranked_where(
                system,
                required_features,
                self.backends.inner.permit_wait,
                |target| {
                    target.kind() != BackendKind::StaticSsh
                        || (self
                            .backends
                            .inner
                            .schedulable_static_ssh
                            .read()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .contains(target.name())
                            && self
                                .backends
                                .inner
                                .static_ssh_health
                                .is_ready(target.name()))
                },
                |target| {
                    producers
                        .iter()
                        .filter(|producer| {
                            producer.backend_name == target.name()
                                && producer.backend_kind == target.kind()
                        })
                        .count()
                },
            )
            .map(Some)
    }
}

impl BuildBackend for BackendExecutor {
    fn compatible_live_log_queue_bytes(&self, system: &str, required_features: &[&str]) -> usize {
        self.backends
            .inner
            .pool
            .targets()
            .filter(|target| target.supports(system, required_features))
            .map(|target| self.live_log_queue_bytes(target))
            .max()
            .unwrap_or(1)
    }

    fn live_log_queue_bytes(&self, target: &crate::backend::BackendTarget) -> usize {
        if target.kind() != BackendKind::Nomad {
            return 64 * 1024;
        }
        self.backends
            .inner
            .nomad
            .iter()
            .find(|config| config.target().name() == target.name())
            .map(|config| config.transfer_limits().live_log_queue_bytes())
            .unwrap_or(1)
    }

    fn execution_id(
        &self,
        target: &crate::backend::BackendTarget,
        shared_build_key: &[u8],
    ) -> io::Result<Option<String>> {
        match target.kind() {
            BackendKind::Nomad => {
                let config = self
                    .backends
                    .inner
                    .nomad
                    .iter()
                    .find(|config| config.target().name() == target.name())
                    .ok_or_else(|| io::Error::other("selected backend is not configured"))?;
                Ok(Some(crate::nomad::backend::deterministic_job_name(
                    config,
                    shared_build_key,
                )))
            }
            BackendKind::Local | BackendKind::StaticSsh => Ok(None),
        }
    }

    fn reserve_target(
        &self,
        system: &str,
        required_features: &[&str],
    ) -> io::Result<Option<crate::backend::BackendPermit>> {
        self.reserve_with_producers(system, required_features, &[])
    }

    fn reserve_build(
        &self,
        execution: &BuildExecution<'_>,
        required_features: &[&str],
    ) -> io::Result<Option<crate::backend::BackendPermit>> {
        self.reserve_with_producers(
            execution.build().system(),
            required_features,
            execution.producers(),
        )
    }

    fn selected_target(
        &self,
        system: &str,
        required_features: &[&str],
    ) -> io::Result<crate::backend::BackendTarget> {
        let selected = self
            .backends
            .inner
            .pool
            .targets()
            .filter(|target| {
                target.supports(system, required_features)
                    && (target.kind() != BackendKind::StaticSsh
                        || (self
                            .backends
                            .inner
                            .schedulable_static_ssh
                            .read()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .contains(target.name())
                            && self
                                .backends
                                .inner
                                .static_ssh_health
                                .is_ready(target.name())))
            })
            .fold(
                None::<&crate::backend::BackendTarget>,
                |selected, target| match selected {
                    Some(current)
                        if current.selection_priority() >= target.selection_priority() =>
                    {
                        selected
                    }
                    _ => Some(target),
                },
            )
            .cloned();
        match selected {
            Some(target) => {
                if target.kind() == BackendKind::Nomad {
                    let config = self
                        .backends
                        .inner
                        .nomad
                        .iter()
                        .find(|config| config.target().name() == target.name())
                        .ok_or_else(|| io::Error::other("selected backend is not configured"))?;
                    config
                        .select_resource_profile(required_features)
                        .map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "Nomad resource profile selection is ambiguous",
                            )
                        })?;
                }
                tracing::debug!(
                    event = "backend.routing.selected",
                    backend_name = target.name(),
                    backend_kind = target.kind().as_str(),
                    system,
                    required_feature_count = required_features.len(),
                    "build backend selected"
                );
                Ok(target)
            }
            None => {
                tracing::debug!(
                    event = "backend.routing.unavailable",
                    system,
                    required_feature_count = required_features.len(),
                    "no compatible build backend available"
                );
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "BuildDerivation execution is unavailable",
                ))
            }
        }
    }

    fn execute_with_logs(
        &mut self,
        execution: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        let target_name = execution
            .target_name()
            .ok_or_else(|| io::Error::other("selected backend is unavailable"))?;
        let permit = self
            .backends
            .inner
            .pool
            .acquire_target(target_name, self.backends.inner.permit_wait)?;
        self.execute_reserved(execution, permit, logs, cancelled)
    }

    fn execute_reserved(
        &mut self,
        execution: &BuildExecution<'_>,
        permit: crate::backend::BackendPermit,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        self.database.wait_for_ownership()?;
        let target_name = permit.target().name().to_owned();
        let target_kind = permit.target().kind();
        let execution_span = tracing::info_span!(
            "backend.execution",
            backend_name = target_name,
            backend_kind = target_kind.as_str()
        );
        execution.trace_context().set_parent(&execution_span);
        let _execution_entered = execution_span.enter();
        let mut execution = execution.clone();
        let routed_trace_context = telchar_telemetry::TraceContext::capture_current();
        if routed_trace_context.trace_id().is_some() {
            execution.set_trace_context(routed_trace_context);
        }
        let execution = &execution;
        let started = std::time::Instant::now();
        tracing::debug!(
            event = "backend.execution.started",
            backend_name = target_name,
            backend_kind = target_kind.as_str(),
            "backend execution started"
        );
        let mut backend: Box<dyn BuildBackend> = match target_kind {
            BackendKind::Local => match (
                &self.backends.inner.local_build_helper,
                &self.backends.inner.gateway_store,
            ) {
                (Some(helper), Some(endpoint)) => Box::new(
                    crate::backend::local::NixStoreExecutor::new(helper, endpoint.to_string())?,
                ),
                (None, Some(endpoint)) => Box::new(
                    crate::backend::local::GatewayStoreExecutor::new(endpoint.clone()),
                ),
                _ => Box::new(crate::backend::local::UnavailableBuildExecutor),
            },
            BackendKind::StaticSsh => {
                let config = self
                    .backends
                    .inner
                    .static_ssh
                    .iter()
                    .find(|config| config.target().name() == permit.target().name())
                    .ok_or_else(|| io::Error::other("selected backend is not configured"))?;
                let gateway_store =
                    self.backends.inner.gateway_store.as_ref().ok_or_else(|| {
                        io::Error::other("gateway store endpoint is not configured")
                    })?;
                Box::new(crate::backend::static_ssh::StaticSshBackend::new(
                    config.clone(),
                    gateway_store.clone(),
                    self.backends.inner.object_admission.clone(),
                ))
            }
            BackendKind::Nomad => {
                let config = self
                    .backends
                    .inner
                    .nomad
                    .iter()
                    .find(|config| config.target().name() == permit.target().name())
                    .ok_or_else(|| io::Error::other("selected backend is not configured"))?;
                let shared_build_key = execution.build().shared_build_key();
                let result = crate::nomad::backend::NomadClient::new(config.clone())?.execute(
                    &self.database,
                    execution,
                    shared_build_key.as_bytes(),
                    logs,
                    &self.shared_builds,
                    config.transfer_limits().live_log_queue_bytes(),
                    cancelled,
                );
                let liability = (|| {
                    let pending =
                        crate::persistence::read_nomad_execution_obligations(&self.database)
                            .map_err(|error| io::Error::other(error.to_string()))?;
                    let derivation = std::str::from_utf8(execution.build().derivation_path())
                        .map_err(|_| io::Error::other("Nomad derivation path is invalid"))?;
                    let attempt =
                        crate::persistence::read_shared_build_attempt(&self.database, derivation)
                            .map_err(|error| io::Error::other(error.to_string()))?;
                    Ok::<_, io::Error>(
                        attempt.filter(|attempt| {
                            pending.iter().any(|(id, _)| *id == attempt.attempt_id)
                        }),
                    )
                })();
                let liability = match liability {
                    Ok(attempt) => attempt,
                    Err(error) => {
                        self.backends
                            .inner
                            .uncertain_nomad_permits
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push(permit);
                        return Err(error);
                    }
                };
                if let Some(attempt) = liability {
                    self.backends
                        .inner
                        .nomad_obligations
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(attempt.attempt_id, permit);
                }
                let result_name = if result
                    .as_ref()
                    .is_ok_and(|result| result.failure().is_none())
                {
                    "succeeded"
                } else {
                    "failed"
                };
                let failure_class = result.as_ref().err().map(|error| match error.kind() {
                    io::ErrorKind::TimedOut => "timeout",
                    io::ErrorKind::Interrupted => "cancelled",
                    _ => "infrastructure",
                });
                crate::service::metrics::backend_execution_finished(
                    &target_name,
                    target_kind.as_str(),
                    started.elapsed(),
                    result_name,
                    failure_class,
                );
                tracing::debug!(
                    event = "backend.execution.completed",
                    backend_name = target_name,
                    backend_kind = target_kind.as_str(),
                    result = result_name,
                    failure_class,
                    duration_ms = started.elapsed().as_millis(),
                    "backend execution completed"
                );
                return result;
            }
        };
        let result = backend.execute_with_logs(
            execution,
            &mut |chunk| {
                self.shared_builds
                    .publish_log(&execution.build().shared_build_key(), chunk);
                logs(chunk)
            },
            cancelled,
        );
        let result_name = if result
            .as_ref()
            .is_ok_and(|result| result.failure().is_none())
        {
            "succeeded"
        } else {
            "failed"
        };
        let failure_class = result.as_ref().err().map(|error| match error.kind() {
            io::ErrorKind::TimedOut => "timeout",
            io::ErrorKind::Interrupted => "cancelled",
            _ => "infrastructure",
        });
        crate::service::metrics::backend_execution_finished(
            &target_name,
            target_kind.as_str(),
            started.elapsed(),
            result_name,
            failure_class,
        );
        tracing::debug!(
            event = "backend.execution.completed",
            backend_name = target_name,
            backend_kind = target_kind.as_str(),
            result = result_name,
            failure_class,
            duration_ms = started.elapsed().as_millis(),
            "backend execution completed"
        );
        result
    }
}
