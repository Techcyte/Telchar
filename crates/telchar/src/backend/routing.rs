//! Constructs configured backend executors and dispatches admitted builds to the selected target.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

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
    permit_wait: std::time::Duration,
    gateway_store: Option<GatewayStoreEndpoint>,
    local_build_helper: Option<PathBuf>,
    static_ssh: Vec<StaticSshBackendConfig>,
    nomad: Vec<NomadBackendConfig>,
}

impl ConfiguredBackends {
    pub fn new(
        config: &ServiceConfig,
        gateway_store: impl Into<Option<GatewayStoreEndpoint>>,
    ) -> io::Result<Self> {
        Self::with_local_build_helper(config, gateway_store, None)
    }

    pub fn with_local_build_helper(
        config: &ServiceConfig,
        gateway_store: impl Into<Option<GatewayStoreEndpoint>>,
        local_build_helper: Option<PathBuf>,
    ) -> io::Result<Self> {
        let gateway_store = gateway_store.into();
        let mut targets = Vec::new();
        let mut maximums = Vec::new();
        if let Some(local) = config.local_backend() {
            targets.push(local.target().clone());
            maximums.push(local.maximum_concurrent_builds());
        }
        for backend in config.static_ssh_backends() {
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
                permit_wait: config.backend_permit_wait(),
                gateway_store,
                local_build_helper,
                static_ssh: config.static_ssh_backends().to_vec(),
                nomad: config.nomad_backends().to_vec(),
            }),
        })
    }

    pub fn executor(&self, database_url: &str) -> io::Result<BackendExecutor> {
        if database_url.trim().is_empty() {
            return Err(io::Error::other("database URL is not configured"));
        }
        Ok(BackendExecutor {
            backends: self.clone(),
            database_url: database_url.to_owned(),
        })
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
    database_url: String,
}

impl BuildBackend for BackendExecutor {
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

    fn selected_target(
        &self,
        system: &str,
        required_features: &[&str],
    ) -> io::Result<crate::backend::BackendTarget> {
        self.backends
            .inner
            .pool
            .targets()
            .find(|target| target.supports(system, required_features))
            .cloned()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "BuildDerivation execution is unavailable",
                )
            })
    }

    fn execute_with_logs(
        &mut self,
        execution: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        let required_features = execution
            .build()
            .required_system_features()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let permit = self.backends.inner.pool.acquire(
            execution.build().system(),
            &required_features,
            self.backends.inner.permit_wait,
        )?;
        let target_name = permit.target().name().to_owned();
        let target_kind = permit.target().kind();
        let started = std::time::Instant::now();
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
                    &self.database_url,
                    execution,
                    shared_build_key.as_bytes(),
                    cancelled,
                );
                crate::service::metrics::backend_execution_finished(
                    &target_name,
                    target_kind.as_str(),
                    started.elapsed(),
                    if result.is_ok() {
                        "succeeded"
                    } else {
                        "failed"
                    },
                    result.as_ref().err().map(|error| match error.kind() {
                        io::ErrorKind::TimedOut => "timeout",
                        io::ErrorKind::Interrupted => "cancelled",
                        _ => "infrastructure",
                    }),
                );
                return result;
            }
        };
        let result = backend.execute_with_logs(execution, logs, cancelled);
        crate::service::metrics::backend_execution_finished(
            &target_name,
            target_kind.as_str(),
            started.elapsed(),
            if result.is_ok() {
                "succeeded"
            } else {
                "failed"
            },
            result.as_ref().err().map(|error| match error.kind() {
                io::ErrorKind::TimedOut => "timeout",
                io::ErrorKind::Interrupted => "cancelled",
                _ => "infrastructure",
            }),
        );
        result
    }
}
