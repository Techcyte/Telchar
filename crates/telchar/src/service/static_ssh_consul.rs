//! Maps validated Consul service membership into ordinary static SSH backend entries.

use std::collections::BTreeSet;
use std::io;

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
            || (config.passing_only()
                && entry
                    .checks
                    .iter()
                    .any(|check| check.status != "passing"))
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
