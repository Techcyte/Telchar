//! Maps EC2 membership into ordinary SSH execution targets.

use std::collections::BTreeSet;
use std::io;

use aws_sdk_ec2::types::{Instance, InstanceStateName};

use crate::service::config::{Ec2Address, StaticSshBackendConfig, StaticSshEc2Config};

pub fn map_instances(
    config: &StaticSshEc2Config,
    instances: &[Instance],
) -> io::Result<Vec<StaticSshBackendConfig>> {
    let mut members = Vec::new();
    let mut identities = BTreeSet::new();
    let mut addresses = BTreeSet::new();
    for instance in instances {
        if instance.state().and_then(|state| state.name()) != Some(&InstanceStateName::Running)
            || !config.tags().iter().all(|(key, value)| {
                instance
                    .tags()
                    .iter()
                    .any(|tag| tag.key() == Some(key) && tag.value() == Some(value))
            })
        {
            continue;
        }
        let address = match config.address() {
            Ec2Address::PrivateIp => instance.private_ip_address(),
            Ec2Address::PublicIp => instance.public_ip_address(),
        };
        let Some(address) = address else { continue };
        let address = address
            .parse::<std::net::IpAddr>()
            .map_err(|_| invalid("EC2 instance address is invalid"))?;
        let identity = instance
            .instance_id()
            .ok_or_else(|| invalid("EC2 instance identity is missing"))?;
        let suffix = identity
            .strip_prefix("i-")
            .ok_or_else(|| invalid("EC2 instance identity is invalid"))?;
        if !matches!(suffix.len(), 8 | 17) || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid("EC2 instance identity is invalid"));
        }
        if !identities.insert(identity) || !addresses.insert(address) {
            return Err(invalid("EC2 membership is ambiguous"));
        }
        if members.len() >= 256 {
            return Err(invalid("EC2 membership exceeds limit"));
        }
        members.push(config.member(identity, address)?);
    }
    members.sort_by(|left, right| left.target().name().cmp(right.target().name()));
    Ok(members)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
