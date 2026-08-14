//! Constructs configured backend executors and dispatches admitted builds to the selected target.

use std::io;
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
    gateway_store: GatewayStoreEndpoint,
    static_ssh: Vec<StaticSshBackendConfig>,
    nomad: Vec<NomadBackendConfig>,
}

impl ConfiguredBackends {
    pub fn new(config: &ServiceConfig, gateway_store: GatewayStoreEndpoint) -> io::Result<Self> {
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
        crate::backend::static_ssh::recover_outputs(
            config,
            &self.inner.gateway_store,
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
            crate::nomad::backend::NomadExecutionState::Monitoring => {
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
        let mut backend: Box<dyn BuildBackend> = match permit.target().kind() {
            BackendKind::Local => Box::new(crate::backend::local::GatewayStoreExecutor::new(
                self.backends.inner.gateway_store.clone(),
            )),
            BackendKind::StaticSsh => {
                let config = self
                    .backends
                    .inner
                    .static_ssh
                    .iter()
                    .find(|config| config.target().name() == permit.target().name())
                    .ok_or_else(|| io::Error::other("selected backend is not configured"))?;
                Box::new(crate::backend::static_ssh::StaticSshBackend::new(
                    config.clone(),
                    self.backends.inner.gateway_store.clone(),
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
                return crate::nomad::backend::NomadClient::new(config.clone())?.execute(
                    &self.database_url,
                    execution,
                    shared_build_key.as_bytes(),
                    cancelled,
                );
            }
        };
        backend.execute_with_logs(execution, logs, cancelled)
    }
}
