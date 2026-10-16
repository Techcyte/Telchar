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
    pub fn run_configured(
        &self,
        config: &crate::service::config::MicrovmBackendConfig,
        attempt_id: i64,
        log: impl FnMut(&[u8]),
    ) -> io::Result<BuildResultMetadata> {
        self.owner.verify().map_err(io::Error::other)?;
        let deadline = Instant::now()
            .checked_add(config.execution_timeout())
            .ok_or_else(|| io::Error::other("MicroVM execution deadline overflow"))?;
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
