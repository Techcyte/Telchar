//! Applies transactional additive static SSH configuration reloads.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::backend::routing::{ConfiguredBackends, ReloadableBackends};
use crate::service::config::ServiceConfig;
use crate::service::daemon_services::StaticSshHealthService;
use crate::store::daemon::GatewayStoreEndpoint;

pub struct BackendReload {
    config: ServiceConfig,
    backends: ConfiguredBackends,
    health_service: StaticSshHealthService,
    added: usize,
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
        let added = current.validate_additive_static_ssh_reload(&config)?;
        let health =
            crate::backend::static_ssh::StaticSshHealth::probe_all(config.static_ssh_backends());
        let backends = ConfiguredBackends::with_health(
            &config,
            gateway_store,
            local_build_helper,
            health.clone(),
        )?;
        let health_service = StaticSshHealthService::start(health, health_interval)?;
        Ok(Self {
            config,
            backends,
            health_service,
            added,
        })
    }

    pub fn apply(
        self,
        current: &mut ServiceConfig,
        backends: &ReloadableBackends,
        health_service: &mut StaticSshHealthService,
    ) -> io::Result<usize> {
        health_service.replace(self.health_service)?;
        backends.replace(self.backends);
        *current = self.config;
        Ok(self.added)
    }
}
