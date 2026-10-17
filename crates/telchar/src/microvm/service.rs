//! Owns the callback and cleanup lifecycle used by scheduled executions.

use std::io;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, TryLockError};

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
    stopped: Arc<AtomicBool>,
}

struct Lifecycle {
    callback: Arc<Mutex<CallbackService>>,
    cleanup: MicrovmCleanupService,
}

impl Service {
    pub fn start(
        config: MicrovmBackendConfig,
        provider: Provider,
        owner: Arc<SingletonOwnership>,
        listener: TcpListener,
        store: GatewayStoreEndpoint,
    ) -> io::Result<Self> {
        let services = Self::start_many(vec![(config, provider)], owner, listener, store)?;
        Arc::try_unwrap(services.into_iter().next().expect("one configured service"))
            .map_err(|_| io::Error::other("MicroVM service ownership unavailable"))
    }

    pub fn start_many(
        deployments: Vec<(MicrovmBackendConfig, Provider)>,
        owner: Arc<SingletonOwnership>,
        listener: TcpListener,
        store: GatewayStoreEndpoint,
    ) -> io::Result<Vec<Arc<Self>>> {
        owner.verify().map_err(io::Error::other)?;
        let mut names = std::collections::BTreeSet::new();
        let mut maximum = 0usize;
        let mut admission_timeout = std::time::Duration::ZERO;
        for (config, _) in &deployments {
            if !names.insert(config.name()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate MicroVM backend",
                ));
            }
            maximum = maximum
                .checked_add(config.maximum_concurrent_builds())
                .filter(|maximum| *maximum <= 65536)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "MicroVM capacity exceeds limit",
                    )
                })?;
            admission_timeout = admission_timeout.max(config.admission_timeout());
        }
        if maximum == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MicroVM deployments are empty",
            ));
        }
        let sessions = Arc::new(ExecutionSessions::new(maximum).map_err(io::Error::other)?);
        let callback = CallbackService::start(
            listener,
            Arc::clone(&owner),
            Arc::clone(&sessions),
            maximum,
            maximum,
            admission_timeout,
        )?;
        let callback = Arc::new(Mutex::new(callback));
        let stopped = Arc::new(AtomicBool::new(false));
        let mut services = Vec::with_capacity(deployments.len());
        for (config, provider) in deployments {
            let cleanup = MicrovmCleanupService::start(
                Arc::clone(&owner),
                provider.clone(),
                config.account_id().to_owned(),
                config.region().to_owned(),
                config.cleanup_interval(),
                maximum.min(1000),
            )?;
            services.push(Arc::new(Self {
                config,
                provider,
                owner: Arc::clone(&owner),
                sessions: Arc::clone(&sessions),
                store: store.clone(),
                lifecycle: Mutex::new(Lifecycle {
                    callback: Arc::clone(&callback),
                    cleanup,
                }),
                stopped: Arc::clone(&stopped),
            }));
        }
        Ok(services)
    }

    pub fn configuration(&self) -> &MicrovmBackendConfig {
        &self.config
    }

    pub fn check(&self) -> io::Result<()> {
        self.owner.verify().map_err(io::Error::other)?;
        let mut lifecycle = loop {
            if self.stopped.load(Ordering::Acquire) {
                return Err(io::Error::other("MicroVM service stopped"));
            }
            match self.lifecycle.try_lock() {
                Ok(lifecycle) => break lifecycle,
                Err(TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(1))
                }
                Err(TryLockError::Poisoned(_)) => {
                    return Err(io::Error::other("MicroVM lifecycle unavailable"));
                }
            }
        };
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::Error::other("MicroVM service stopped"));
        }
        let callback = Arc::clone(&lifecycle.callback);
        let mut callback = loop {
            if self.stopped.load(Ordering::Acquire) {
                return Err(io::Error::other("MicroVM service stopped"));
            }
            match callback.try_lock() {
                Ok(callback) => break callback,
                Err(TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(1))
                }
                Err(TryLockError::Poisoned(_)) => {
                    return Err(io::Error::other("MicroVM listener unavailable"));
                }
            }
        };
        callback.check()?;
        lifecycle.cleanup.check()
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.stopped.store(true, Ordering::Release);
        let mut lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| io::Error::other("MicroVM lifecycle unavailable"))?;
        let callback = lifecycle
            .callback
            .lock()
            .map_err(|_| io::Error::other("MicroVM listener unavailable"))?
            .shutdown();
        let cleanup = lifecycle.cleanup.shutdown();
        callback.and(cleanup)
    }

    fn execute_build(
        &self,
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

impl BuildBackend for Arc<Service> {
    fn execute_with_logs(
        &mut self,
        build: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        self.execute_build(build, logs, cancelled)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
