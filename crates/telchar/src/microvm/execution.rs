//! Dispatches one reserved execution and retains its durable cleanup obligation.

use std::io;
use std::path::PathBuf;
use std::time::Instant;

use super::Provider;
use super::rendezvous::ExecutionSessions;
use super::session::{Limits, run_admitted};
use crate::nomad::protocol::BuildResultMetadata;
use crate::persistence::MicrovmLaunchIntent;
use crate::service::singleton_ownership::SingletonOwnership;
use crate::store::daemon::GatewayStoreEndpoint;

pub struct Execution<'a> {
    pub provider: &'a Provider,
    pub owner: &'a SingletonOwnership,
    pub sessions: &'a ExecutionSessions,
    pub store: GatewayStoreEndpoint,
    pub staging: PathBuf,
    pub limits: Limits,
}

impl Execution<'_> {
    pub fn execute_build(
        &self,
        config: &crate::service::config::MicrovmBackendConfig,
        build: &crate::backend::BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<crate::backend::BuildResult> {
        self.owner.verify().map_err(io::Error::other)?;
        if build.target_name() != Some(config.name()) {
            return Err(io::Error::other(
                "MicroVM selected target does not match deployment",
            ));
        }
        if cancelled()? {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "MicroVM build cancelled",
            ));
        }
        let derivation =
            std::str::from_utf8(build.build().derivation_path()).map_err(io::Error::other)?;
        let attempt =
            crate::persistence::read_shared_build_attempt(self.owner.database(), derivation)
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("MicroVM attempt is missing"))?;
        if attempt.backend_name != config.name() {
            return Err(io::Error::other(
                "MicroVM attempt backend does not match deployment",
            ));
        }
        std::thread::scope(|scope| {
            let (sender, receiver) = std::sync::mpsc::sync_channel::<Vec<u8>>(4);
            let worker = std::thread::Builder::new()
                .name("microvm-execution".into())
                .spawn_scoped(scope, move || {
                    self.run_build(config, build, |bytes| {
                        for chunk in bytes.chunks(64 * 1024) {
                            let _ = sender.try_send(chunk.to_vec());
                        }
                    })
                })?;
            let mut failure = None;
            while !worker.is_finished() {
                match receiver.recv_timeout(std::time::Duration::from_millis(50)) {
                    Ok(bytes) if failure.is_none() => {
                        if let Err(error) = logs(&bytes) {
                            failure = Some(error);
                        }
                    }
                    _ => {}
                }
                if failure.is_none() {
                    match cancelled() {
                        Ok(false) => {}
                        Ok(true) => {
                            failure = Some(io::Error::new(
                                io::ErrorKind::Interrupted,
                                "MicroVM build cancelled",
                            ))
                        }
                        Err(error) => failure = Some(error),
                    }
                }
                if failure.is_some() {
                    self.owner.verify().map_err(io::Error::other)?;
                    crate::persistence::cancel_microvm_attempt(
                        self.owner.database(),
                        attempt.attempt_id,
                    )
                    .map_err(io::Error::other)?;
                }
            }
            let result = worker
                .join()
                .map_err(|_| io::Error::other("MicroVM execution thread failed"))?;
            while let Ok(bytes) = receiver.try_recv() {
                if failure.is_none()
                    && let Err(error) = logs(&bytes)
                {
                    failure = Some(error);
                }
            }
            match failure {
                Some(error) => Err(error),
                None => result,
            }
        })
    }

    pub fn run_build(
        &self,
        config: &crate::service::config::MicrovmBackendConfig,
        build: &crate::backend::BuildExecution<'_>,
        log: impl FnMut(&[u8]),
    ) -> io::Result<crate::backend::BuildResult> {
        self.owner.verify().map_err(io::Error::other)?;
        if build.target_name() != Some(config.name()) {
            return Err(io::Error::other(
                "MicroVM selected target does not match deployment",
            ));
        }
        let deadline = Instant::now()
            .checked_add(build.timeout().min(config.execution_timeout()))
            .ok_or_else(|| io::Error::other("MicroVM execution deadline overflow"))?;
        let derivation =
            std::str::from_utf8(build.build().derivation_path()).map_err(io::Error::other)?;
        let attempt =
            crate::persistence::read_shared_build_attempt(self.owner.database(), derivation)
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("MicroVM attempt is missing"))?;
        let request = crate::persistence::microvm_attempt_request(
            self.owner.database(),
            attempt.attempt_id,
            config.name(),
        )
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::other("MicroVM attempt is not active"))?;
        if &request != build.build() {
            return Err(io::Error::other(
                "MicroVM admitted request does not match execution",
            ));
        }
        let result = self.run_until(config, attempt.attempt_id, deadline, log)?;
        if result.outcome != crate::nomad::protocol::BuildOutcome::Built {
            return Err(io::Error::other("MicroVM build failed"));
        }
        crate::backend::BuildResult::new(
            crate::backend::BuildStatus::Built,
            request.expected_outputs().to_vec(),
            crate::backend::OutputTrust::TrustedExecutor,
        )
    }

    pub fn run_configured(
        &self,
        config: &crate::service::config::MicrovmBackendConfig,
        attempt_id: i64,
        log: impl FnMut(&[u8]),
    ) -> io::Result<BuildResultMetadata> {
        let deadline = Instant::now()
            .checked_add(config.execution_timeout())
            .ok_or_else(|| io::Error::other("MicroVM execution deadline overflow"))?;
        self.run_until(config, attempt_id, deadline, log)
    }

    fn run_until(
        &self,
        config: &crate::service::config::MicrovmBackendConfig,
        attempt_id: i64,
        deadline: Instant,
        log: impl FnMut(&[u8]),
    ) -> io::Result<BuildResultMetadata> {
        self.owner.verify().map_err(io::Error::other)?;
        let request = crate::persistence::microvm_attempt_request(
            self.owner.database(),
            attempt_id,
            config.name(),
        )
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::other("MicroVM attempt is not active"))?;
        if request.system() != "aarch64-linux" || !request.required_system_features().is_empty() {
            return Err(io::Error::other(
                "MicroVM request is not compatible with deployment",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("MicroVM execution deadline expired"));
        }
        let (intent, payload) = config.launch(attempt_id)?;
        if !crate::persistence::reserve_microvm_launch(
            self.owner.database(),
            &intent,
            config.maximum_concurrent_builds(),
        )
        .map_err(io::Error::other)?
        {
            return Err(io::Error::other("MicroVM reservation was not granted"));
        }
        let execution = Execution {
            provider: self.provider,
            owner: self.owner,
            sessions: self.sessions,
            store: self.store.clone(),
            staging: config.staging_directory().to_owned(),
            limits: config.transfer_limits(),
        };
        let result = execution.run_reserved(&intent, &payload, deadline, log);
        self.owner.verify().map_err(io::Error::other)?;
        crate::persistence::cancel_microvm_launch(self.owner.database(), &intent.intent_id)
            .map_err(io::Error::other)?;
        result
    }

    pub fn run_reserved(
        &self,
        intent: &MicrovmLaunchIntent,
        payload: &str,
        deadline: Instant,
        log: impl FnMut(&[u8]),
    ) -> io::Result<BuildResultMetadata> {
        self.owner.verify().map_err(io::Error::other)?;
        let waiting = self
            .sessions
            .register(&intent.intent_id, deadline)
            .map_err(io::Error::other)?;
        if Instant::now() >= deadline {
            return Err(io::Error::other("MicroVM execution deadline expired"));
        }
        let launched = self.provider.launch_reserved(self.owner, intent, payload);
        if matches!(launched, Ok(None)) {
            return Err(io::Error::other("MicroVM dispatch was not granted"));
        }
        let result = (|| {
            launched.map_err(io::Error::other)?;
            let connection = waiting
                .receive_active(self.owner)
                .map_err(io::Error::other)?;
            run_admitted(
                connection,
                self.owner,
                self.store.clone(),
                self.staging.clone(),
                self.limits,
                log,
            )
        })();
        drop(waiting);
        self.owner.verify().map_err(io::Error::other)?;
        crate::persistence::cancel_microvm_launch(self.owner.database(), &intent.intent_id)
            .map_err(io::Error::other)?;
        result
    }
}
