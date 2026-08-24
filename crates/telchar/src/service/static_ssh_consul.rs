//! Maps validated Consul service membership into ordinary static SSH backend entries.

use std::collections::BTreeSet;
use std::io;
use std::sync::{Arc, RwLock};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::backend::{BackendKind, BackendTarget};
use crate::service::config::{StaticSshBackendConfig, StaticSshConsulConfig};

#[derive(Deserialize)]
struct HealthServiceEntry {
    #[serde(rename = "Node")]
    node: HealthNode,
    #[serde(rename = "Service")]
    service: HealthService,
    #[serde(rename = "Checks", default)]
    checks: Vec<HealthCheck>,
}

#[derive(Deserialize)]
struct HealthNode {
    #[serde(rename = "Node")]
    name: String,
    #[serde(rename = "Address")]
    address: String,
}

#[derive(Deserialize)]
struct HealthService {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Service")]
    name: String,
    #[serde(rename = "Tags", default)]
    tags: Vec<String>,
    #[serde(rename = "Address")]
    address: String,
    #[serde(rename = "Port")]
    port: u16,
}

#[derive(Deserialize)]
struct HealthCheck {
    #[serde(rename = "Status")]
    status: String,
}

pub struct ConsulSshDiscovery {
    config: StaticSshConsulConfig,
    client: reqwest::blocking::Client,
    inventory: Arc<RwLock<Vec<StaticSshBackendConfig>>>,
}

impl ConsulSshDiscovery {
    pub fn new(
        config: StaticSshConsulConfig,
        inventory: Arc<RwLock<Vec<StaticSshBackendConfig>>>,
    ) -> io::Result<Self> {
        let mut builder = reqwest::blocking::Client::builder().timeout(config.request_timeout());
        if let Some(path) = config.ca_certificate_file() {
            let certificate = std::fs::read(path)
                .map_err(|_| io::Error::other("Consul SSH discovery configuration failed"))?;
            let certificate = reqwest::Certificate::from_pem(&certificate)
                .map_err(|_| io::Error::other("Consul SSH discovery configuration failed"))?;
            builder = builder.add_root_certificate(certificate);
        }
        let client = builder
            .build()
            .map_err(|_| io::Error::other("Consul SSH discovery configuration failed"))?;
        Ok(Self {
            config,
            client,
            inventory,
        })
    }

    pub fn refresh(&mut self) -> io::Result<usize> {
        let mut request = self.client.get(format!(
            "{}/v1/health/service/{}",
            self.config.endpoint(),
            self.config.service()
        ));
        let mut query = Vec::new();
        if self.config.passing_only() {
            query.push(("passing", "true".to_owned()));
        }
        if let Some(datacenter) = self.config.datacenter() {
            query.push(("dc", datacenter.to_owned()));
        }
        for tag in self.config.required_tags() {
            query.push(("tag", tag.clone()));
        }
        request = request.query(&query);
        if let Some(path) = self.config.token_file() {
            let token = std::fs::read_to_string(path)
                .map_err(|_| io::Error::other("Consul SSH discovery request failed"))?;
            request = request.header("X-Consul-Token", token.trim());
        }
        let response = request
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|_| io::Error::other("Consul SSH discovery request failed"))?;
        let value = response
            .json::<serde_json::Value>()
            .map_err(|_| io::Error::other("Consul SSH discovery response failed"))?;
        let members = map_members(&self.config, &value)?;
        let count = members.len();
        *self
            .inventory
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = members;
        Ok(count)
    }
}

pub struct ConsulSshDiscoveryService {
    stop: Option<std::sync::mpsc::Sender<()>>,
    updates: std::sync::mpsc::Receiver<Vec<StaticSshBackendConfig>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ConsulSshDiscoveryService {
    pub fn start(
        configs: Vec<StaticSshConsulConfig>,
        interval: std::time::Duration,
    ) -> io::Result<Self> {
        if configs.is_empty() || interval.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Consul SSH discovery service configuration is invalid",
            ));
        }
        let discoveries = configs
            .into_iter()
            .map(|config| {
                let inventory = Arc::new(RwLock::new(Vec::new()));
                ConsulSshDiscovery::new(config, Arc::clone(&inventory))
                    .map(|discovery| (discovery, inventory))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let (update_tx, update_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut discoveries = discoveries;
            loop {
                let mut changed = false;
                for (discovery, _) in &mut discoveries {
                    if discovery.refresh().is_ok() {
                        changed = true;
                    }
                }
                if changed {
                    let inventory = discoveries
                        .iter()
                        .flat_map(|(_, inventory)| {
                            inventory
                                .read()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .clone()
                        })
                        .collect();
                    let _ = update_tx.send(inventory);
                }
                match stop_rx.recv_timeout(interval) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        Ok(Self {
            stop: Some(stop_tx),
            updates: update_rx,
            worker: Some(worker),
        })
    }

    pub fn check(&mut self) -> io::Result<Option<Vec<StaticSshBackendConfig>>> {
        let mut latest = None;
        loop {
            match self.updates.try_recv() {
                Ok(inventory) => latest = Some(inventory),
                Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(latest),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err(io::Error::other("Consul SSH discovery service failed"));
                }
            }
        }
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        self.stop.take();
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("Consul SSH discovery service failed"))?;
        }
        Ok(())
    }
}

impl Drop for ConsulSshDiscoveryService {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

pub fn replace_manual_inventory(
    config: &mut crate::service::config::ServiceConfig,
    manual: Vec<StaticSshBackendConfig>,
    discovered: &[StaticSshBackendConfig],
    backends: &crate::backend::routing::ReloadableBackends,
    gateway_store: Option<crate::store::daemon::GatewayStoreEndpoint>,
    local_build_helper: Option<std::path::PathBuf>,
) -> io::Result<()> {
    config.replace_static_ssh_backends(manual);
    publish_inventory(
        config,
        discovered,
        backends,
        gateway_store,
        local_build_helper,
    )
}

pub fn publish_inventory(
    config: &crate::service::config::ServiceConfig,
    discovered: &[StaticSshBackendConfig],
    backends: &crate::backend::routing::ReloadableBackends,
    gateway_store: Option<crate::store::daemon::GatewayStoreEndpoint>,
    local_build_helper: Option<std::path::PathBuf>,
) -> io::Result<()> {
    let merged = merge_inventory(config, discovered)?;
    let desired = merged
        .iter()
        .map(|backend| backend.target().name().to_owned())
        .collect::<BTreeSet<_>>();
    let replacement = crate::backend::routing::ConfiguredBackends::with_static_ssh_inventory(
        config,
        merged,
        gateway_store,
        local_build_helper,
    )?;
    backends.disable_static_ssh_not_in(&desired);
    backends.replace(replacement);
    Ok(())
}

pub fn merge_inventory(
    config: &crate::service::config::ServiceConfig,
    discovered: &[StaticSshBackendConfig],
) -> io::Result<Vec<StaticSshBackendConfig>> {
    let mut names = BTreeSet::new();
    let mut merged = Vec::with_capacity(config.static_ssh_backends().len() + discovered.len());
    for backend in config.static_ssh_backends().iter().chain(discovered) {
        if !names.insert(backend.target().name().to_owned()) {
            return Err(invalid("static SSH inventory name is ambiguous"));
        }
        merged.push(backend.clone());
    }
    Ok(merged)
}

pub fn map_members(
    config: &StaticSshConsulConfig,
    response: &serde_json::Value,
) -> io::Result<Vec<StaticSshBackendConfig>> {
    let entries: Vec<HealthServiceEntry> = serde_json::from_value(response.clone())
        .map_err(|_| invalid("Consul SSH membership response is invalid"))?;
    let required_tags = config.required_tags().iter().collect::<BTreeSet<_>>();
    let mut endpoints = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut backends = Vec::new();
    for entry in entries {
        if entry.service.name != config.service()
            || !required_tags
                .iter()
                .all(|required| entry.service.tags.iter().any(|tag| tag == *required))
            || (config.passing_only() && entry.checks.iter().any(|check| check.status != "passing"))
        {
            continue;
        }
        let address = if entry.service.address.is_empty() {
            &entry.node.address
        } else {
            &entry.service.address
        };
        if !valid_identity(&entry.node.name)
            || !valid_identity(&entry.service.id)
            || !valid_address(address)
            || entry.service.port == 0
        {
            return Err(invalid("Consul SSH membership entry is invalid"));
        }
        let endpoint = format!("{}:{}", address, entry.service.port);
        if !endpoints.insert(endpoint) {
            return Err(invalid("Consul SSH membership endpoint is ambiguous"));
        }
        let mut digest = Sha256::new();
        digest.update(entry.node.name.as_bytes());
        digest.update(b"\0");
        digest.update(entry.service.id.as_bytes());
        digest.update(b"\0");
        digest.update(address.as_bytes());
        digest.update(b"\0");
        digest.update(entry.service.port.to_be_bytes());
        let suffix = digest.finalize()[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let name = format!("{}-{suffix}", config.name());
        if !names.insert(name.clone()) {
            return Err(invalid("Consul SSH membership name is ambiguous"));
        }
        let destination = format!("{}@{address}", config.ssh_user());
        backends.push(StaticSshBackendConfig::discovered(
            BackendTarget::new(
                &name,
                BackendKind::StaticSsh,
                config.system(),
                config.supported_features(),
            )?,
            config.maximum_concurrent_builds_per_instance(),
            destination,
            entry.service.port,
            config.identity_file().to_owned(),
            config.known_hosts_file().to_owned(),
            config.ssh_program().to_owned(),
        ));
    }
    backends.sort_by(|left, right| left.target().name().cmp(right.target().name()));
    Ok(backends)
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn valid_address(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
        })
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
