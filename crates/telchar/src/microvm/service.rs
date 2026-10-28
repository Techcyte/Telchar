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
    cleanup: Arc<Mutex<MicrovmCleanupService>>,
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
        let mut cleanup_intervals = std::collections::BTreeMap::new();
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
            let interval = cleanup_intervals
                .entry((config.account_id().to_owned(), config.region().to_owned()))
                .or_insert(config.cleanup_interval());
            *interval = (*interval).min(config.cleanup_interval());
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
        let mut cleanups = std::collections::BTreeMap::new();
        for (config, provider) in deployments {
            let identity = (config.account_id().to_owned(), config.region().to_owned());
            let cleanup = match cleanups.entry(identity.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => Arc::clone(entry.get()),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let cleanup = Arc::new(Mutex::new(MicrovmCleanupService::start(
                        Arc::clone(&owner),
                        provider.clone(),
                        identity.0.clone(),
                        identity.1.clone(),
                        cleanup_intervals[&identity],
                        maximum.min(1000),
                    )?));
                    entry.insert(Arc::clone(&cleanup));
                    cleanup
                }
            };
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
        self.check_request(None)
    }

    fn check_request(&self, scope: Option<&crate::service::admin::RequestScope>) -> io::Result<()> {
        if let Some(scope) = scope {
            scope.remaining()?;
            self.owner
                .verify_database(&scope.database(self.owner.database()))
                .map_err(io::Error::other)?;
        } else {
            self.owner.verify().map_err(io::Error::other)?;
        }
        let lifecycle = loop {
            if let Some(scope) = scope {
                scope.remaining()?;
            }
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
            if let Some(scope) = scope {
                scope.remaining()?;
            }
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
        loop {
            if let Some(scope) = scope {
                scope.remaining()?;
            }
            if self.stopped.load(Ordering::Acquire) {
                return Err(io::Error::other("MicroVM service stopped"));
            }
            match lifecycle.cleanup.try_lock() {
                Ok(mut cleanup) => return cleanup.check(),
                Err(TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(1))
                }
                Err(TryLockError::Poisoned(_)) => {
                    return Err(io::Error::other("MicroVM cleanup unavailable"));
                }
            }
        }
    }

    pub fn verify_termination(
        &self,
        scope: &crate::service::admin::RequestScope,
        observation: &crate::persistence::MicrovmTerminationObservation<'_>,
    ) -> io::Result<bool> {
        use crate::persistence::valid_resolution_text;
        if observation.actor_uid != 0
            || !valid_resolution_text(observation.intent_id, 128)
            || !valid_resolution_text(observation.provider_id, 256)
            || !valid_resolution_text(observation.reason, 1024)
            || !valid_resolution_text(observation.evidence_reference, 1024)
            || observation.account_id != self.config.account_id()
            || observation.region != self.config.region()
        {
            return Err(io::Error::other("invalid MicroVM termination observation"));
        }
        self.verify_administrative_provider(
            scope,
            self.provider.verify_termination(
                observation.provider_id,
                observation.account_id,
                observation.region,
            ),
        )?;
        crate::persistence::resolve_microvm_termination(
            &scope.database(self.owner.database()),
            observation,
        )
        .map_err(io::Error::other)
    }

    fn verify_administrative_provider(
        &self,
        scope: &crate::service::admin::RequestScope,
        verification: impl std::future::Future<Output = Result<(), super::ProviderError>>,
    ) -> io::Result<()> {
        self.check_request(Some(scope))?;
        let timeout = scope.remaining()?.min(std::time::Duration::from_secs(2));
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                tokio::time::timeout(timeout, async {
                    tokio::pin!(verification);
                    loop {
                        scope.remaining()?;
                        tokio::select! {
                            result = &mut verification => return result.map_err(io::Error::other),
                            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
                        }
                    }
                })
                .await
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "MicroVM administrative provider verification timed out",
                )
            })?
            .map_err(io::Error::other)?;
        self.check_request(Some(scope))
    }

    pub fn attach_identity(
        &self,
        scope: &crate::service::admin::RequestScope,
        attachment: &crate::persistence::MicrovmIdentityAttachment<'_>,
    ) -> io::Result<bool> {
        use crate::persistence::valid_resolution_text;
        if attachment.actor_uid != 0
            || !valid_resolution_text(attachment.intent_id, 128)
            || !valid_resolution_text(attachment.provider_id, 256)
            || !valid_resolution_text(attachment.reason, 1024)
            || !valid_resolution_text(attachment.evidence_reference, 1024)
            || attachment.account_id != self.config.account_id()
            || attachment.region != self.config.region()
        {
            return Err(io::Error::other("invalid MicroVM identity attachment"));
        }
        self.verify_administrative_provider(
            scope,
            self.provider.verify_identity(
                attachment.provider_id,
                attachment.account_id,
                attachment.region,
                attachment.image_arn,
                attachment.image_version,
            ),
        )?;
        crate::persistence::attach_microvm_cleanup_identity(
            &scope.database(self.owner.database()),
            attachment,
        )
        .map_err(io::Error::other)
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.stopped.store(true, Ordering::Release);
        let lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| io::Error::other("MicroVM lifecycle unavailable"))?;
        let callback = lifecycle
            .callback
            .lock()
            .map_err(|_| io::Error::other("MicroVM listener unavailable"))?
            .shutdown();
        let cleanup = lifecycle
            .cleanup
            .lock()
            .map_err(|_| io::Error::other("MicroVM cleanup unavailable"))?
            .shutdown();
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
