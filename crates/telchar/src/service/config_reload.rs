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
        discovered: &[crate::service::config::StaticSshBackendConfig],
        gateway_store: Option<GatewayStoreEndpoint>,
        local_build_helper: Option<PathBuf>,
        health_interval: Duration,
    ) -> io::Result<Self> {
        Self::prepare_config_with_inventory(
            current,
            ServiceConfig::load()?,
            discovered,
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
        Self::prepare_config_with_inventory(
            current,
            config,
            &[],
            gateway_store,
            local_build_helper,
            health_interval,
        )
    }

    pub fn prepare_config_with_inventory(
        current: &ServiceConfig,
        config: ServiceConfig,
        discovered: &[crate::service::config::StaticSshBackendConfig],
        gateway_store: Option<GatewayStoreEndpoint>,
        local_build_helper: Option<PathBuf>,
        health_interval: Duration,
    ) -> io::Result<Self> {
        let changes = current.validate_static_ssh_reload(&config)?;
        let inventory = crate::service::static_ssh_consul::merge_inventory(&config, discovered)?;
        let health = crate::backend::static_ssh::StaticSshHealth::probe_all(&inventory);
        let desired_static_ssh = inventory
            .iter()
            .map(|backend| backend.target().name().to_owned())
            .collect::<BTreeSet<_>>();
        let schedulable_static_ssh = Arc::new(RwLock::new(desired_static_ssh.clone()));
        let backends = ConfiguredBackends::with_static_ssh_inventory_and_health(
            &config,
            inventory,
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

    #[doc(hidden)]
    pub fn target_names(&self) -> Vec<String> {
        self.backends
            .targets()
            .map(|target| target.name().to_owned())
            .collect()
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
