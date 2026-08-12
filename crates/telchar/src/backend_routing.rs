use std::io;
use std::sync::Arc;

use crate::backend::{
    BackendCapabilities, BackendKind, BackendPool, BuildBackend, BuildExecution, BuildResult,
};
use crate::config::{NomadBackendConfig, ServiceConfig, StaticSshBackendConfig};
use crate::store_daemon::GatewayStoreEndpoint;

#[derive(Clone)]
pub struct ConfiguredBackends {
    inner: Arc<ConfiguredBackendsInner>,
}

struct ConfiguredBackendsInner {
    pool: BackendPool,
    permit_wait: std::time::Duration,
    static_ssh: Vec<StaticSshBackendConfig>,
    nomad: Vec<NomadBackendConfig>,
}

impl ConfiguredBackends {
    pub fn new(config: &ServiceConfig) -> io::Result<Self> {
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
                static_ssh: config.static_ssh_backends().to_vec(),
                nomad: config.nomad_backends().to_vec(),
            }),
        })
    }

    pub fn executor(&self) -> BackendExecutor {
        BackendExecutor {
            backends: self.clone(),
        }
    }
}

impl crate::shared_build_recovery::RecoveryBackend for ConfiguredBackends {
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
        let endpoint = std::env::var_os("TELCHAR_GATEWAY_STORE_URI").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "gateway store endpoint is not configured",
            )
        })?;
        crate::static_ssh_backend::recover_outputs(
            config,
            &GatewayStoreEndpoint::parse_os(&endpoint)?,
            &build.expected_outputs,
            std::time::Duration::from_secs(10),
        )?;
        Ok(true)
    }

    fn adopt(
        &mut self,
        build: &crate::persistence::SharedBuild,
    ) -> io::Result<crate::shared_build_recovery::AdoptedExecution> {
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
        match crate::nomad_backend::NomadClient::new(config.clone())?.status(execution_id)? {
            crate::nomad_backend::NomadExecutionState::Monitoring => {
                Ok(crate::shared_build_recovery::AdoptedExecution::Monitoring)
            }
            crate::nomad_backend::NomadExecutionState::Succeeded => {
                Ok(crate::shared_build_recovery::AdoptedExecution::Succeeded)
            }
            crate::nomad_backend::NomadExecutionState::Failed => {
                Ok(crate::shared_build_recovery::AdoptedExecution::Failed)
            }
            crate::nomad_backend::NomadExecutionState::Missing => {
                Ok(crate::shared_build_recovery::AdoptedExecution::Missing)
            }
        }
    }
}

pub struct BackendExecutor {
    backends: ConfiguredBackends,
}

impl BuildBackend for BackendExecutor {
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
            BackendKind::Local => crate::local_executor::executor_from_environment()?,
            BackendKind::StaticSsh => {
                let config = self
                    .backends
                    .inner
                    .static_ssh
                    .iter()
                    .find(|config| config.target().name() == permit.target().name())
                    .ok_or_else(|| io::Error::other("selected backend is not configured"))?;
                let endpoint = std::env::var_os("TELCHAR_GATEWAY_STORE_URI").ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "gateway store endpoint is not configured",
                    )
                })?;
                Box::new(crate::static_ssh_backend::StaticSshBackend::new(
                    config.clone(),
                    GatewayStoreEndpoint::parse_os(&endpoint)?,
                ))
            }
            BackendKind::Nomad => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "BuildDerivation execution is unavailable",
                ));
            }
        };
        backend.execute_with_logs(execution, logs, cancelled)
    }
}
