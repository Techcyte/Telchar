//! Applies transactional additive static SSH configuration reloads.

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::backend::routing::{ConfiguredBackends, ReloadableBackends};
use crate::service::config::ServiceConfig;
use crate::service::daemon_services::StaticSshHealthService;
use crate::store::daemon::GatewayStoreEndpoint;

pub struct BackendReload {
    config: ServiceConfig,
    backends: ConfiguredBackends,
    health_service: StaticSshHealthService,
    changes: crate::service::config::StaticSshReloadChanges,
    desired_static_ssh: BTreeSet<String>,
}

impl BackendReload {
    pub fn prepare(
        current: &ServiceConfig,
        gateway_store: Option<GatewayStoreEndpoint>,
        local_build_helper: Option<PathBuf>,
        health_interval: Duration,
    ) -> io::Result<Self> {
        Self::prepare_config(
            current,
            ServiceConfig::load()?,
            gateway_store,
            local_build_helper,
            health_interval,
        )
    }

    pub fn prepare_config(
        current: &ServiceConfig,
        config: ServiceConfig,
        gateway_store: Option<GatewayStoreEndpoint>,
        local_build_helper: Option<PathBuf>,
        health_interval: Duration,
    ) -> io::Result<Self> {
        let changes = current.validate_static_ssh_reload(&config)?;
        let health =
            crate::backend::static_ssh::StaticSshHealth::probe_all(config.static_ssh_backends());
        let desired_static_ssh = config
            .static_ssh_backends()
            .iter()
            .map(|backend| backend.target().name().to_owned())
            .collect::<BTreeSet<_>>();
        let schedulable_static_ssh = Arc::new(RwLock::new(desired_static_ssh.clone()));
        let backends = ConfiguredBackends::with_health_and_scheduling(
            &config,
            gateway_store,
            local_build_helper,
            health.clone(),
            schedulable_static_ssh,
        )?;
        let health_service = StaticSshHealthService::start(health, health_interval)?;
        Ok(Self {
            config,
            backends,
            health_service,
            changes,
            desired_static_ssh,
        })
    }

    pub fn apply(
        self,
        current: &mut ServiceConfig,
        backends: &ReloadableBackends,
        health_service: &mut StaticSshHealthService,
    ) -> io::Result<crate::service::config::StaticSshReloadChanges> {
        backends.disable_static_ssh_not_in(&self.desired_static_ssh);
        health_service.replace(self.health_service)?;
        backends.replace(self.backends);
        *current = self.config;
        Ok(self.changes)
    }
}
