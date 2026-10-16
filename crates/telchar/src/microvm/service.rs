//! Owns the callback and cleanup lifecycle used by scheduled executions.

use std::io;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::Provider;
use super::callback_service::CallbackService;
use super::execution::Execution;
use super::rendezvous::ExecutionSessions;
use crate::backend::{BuildBackend, BuildExecution, BuildResult};
use crate::service::config::MicrovmBackendConfig;
use crate::service::daemon_services::MicrovmCleanupService;
use crate::service::singleton_ownership::SingletonOwnership;
use crate::store::daemon::GatewayStoreEndpoint;

pub struct Service {
    config: MicrovmBackendConfig,
    provider: Provider,
    owner: Arc<SingletonOwnership>,
    sessions: Arc<ExecutionSessions>,
    store: GatewayStoreEndpoint,
    lifecycle: Mutex<Lifecycle>,
}

struct Lifecycle {
    callback: CallbackService,
    cleanup: MicrovmCleanupService,
    stopped: bool,
}

impl Service {
    pub fn start(
        config: MicrovmBackendConfig,
        provider: Provider,
        owner: Arc<SingletonOwnership>,
        listener: TcpListener,
        store: GatewayStoreEndpoint,
    ) -> io::Result<Self> {
        owner.verify().map_err(io::Error::other)?;
        let maximum = config.maximum_concurrent_builds();
        let sessions = Arc::new(ExecutionSessions::new(maximum).map_err(io::Error::other)?);
        let callback = CallbackService::start(
            listener,
            Arc::clone(&owner),
            Arc::clone(&sessions),
            maximum,
            maximum,
            config.admission_timeout(),
        )?;
        let cleanup = MicrovmCleanupService::start(
            Arc::clone(&owner),
            provider.clone(),
            config.name().to_owned(),
            config.cleanup_interval(),
            maximum.min(1000),
        )?;
        Ok(Self {
            config,
            provider,
            owner,
            sessions,
            store,
            lifecycle: Mutex::new(Lifecycle {
                callback,
                cleanup,
                stopped: false,
            }),
        })
    }

    pub fn check(&self) -> io::Result<()> {
        self.owner.verify().map_err(io::Error::other)?;
        let mut lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| io::Error::other("MicroVM lifecycle unavailable"))?;
        if lifecycle.stopped {
            return Err(io::Error::other("MicroVM service stopped"));
        }
        lifecycle.callback.check()?;
        lifecycle.cleanup.check()
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        let lifecycle = self
            .lifecycle
            .get_mut()
            .map_err(|_| io::Error::other("MicroVM lifecycle unavailable"))?;
        lifecycle.stopped = true;
        let callback = lifecycle.callback.shutdown();
        let cleanup = lifecycle.cleanup.shutdown();
        callback.and(cleanup)
    }
}

impl BuildBackend for Service {
    fn execute_with_logs(
        &mut self,
        build: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        self.check()?;
        Execution {
            provider: &self.provider,
            owner: &self.owner,
            sessions: &self.sessions,
            store: self.store.clone(),
            staging: self.config.staging_directory().to_owned(),
            limits: self.config.transfer_limits(),
        }
        .execute_build(&self.config, build, logs, &mut || {
            self.check()?;
            cancelled()
        })
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
