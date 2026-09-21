//! EC2 membership discovery configuration contracts.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use telchar::service::config::ServiceConfig;

fn configuration(root: &std::path::Path, source: &str) -> std::path::PathBuf {
    let identity = root.join("identity");
    let known_hosts = root.join("known-hosts");
    let ssh = root.join("ssh");
    fs::write(&identity, "private").unwrap();
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&known_hosts, "@cert-authority * ssh-ed25519 AAAA\n").unwrap();
    fs::write(&ssh, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    let path = root.join("telchar.toml");
    fs::write(
        &path,
        format!(
            r#"
[[backends.ssh]]
system = "x86_64-linux"
maximum_concurrent_builds = 2
identity_file = "{}"
known_hosts_file = "{}"
ssh_program = "{}"

[backends.ssh.builders]
{source}
"#,
            identity.display(),
            known_hosts.display(),
            ssh.display()
        ),
    )
    .unwrap();
    path
}

#[test]
fn maps_only_running_tagged_instances_with_selected_addresses() {
    use aws_sdk_ec2::types::{Instance, InstanceState, InstanceStateName, Tag};
    let root = tempfile::tempdir().unwrap();
    let path = configuration(
        root.path(),
        r#"
source = "ec2"
region = "us-east-1"
[backends.ssh.builders.tags]
telchar-pool = "builders"
"#,
    );
    let config = ServiceConfig::load_from_default(&path).unwrap();
    let instance = Instance::builder()
        .instance_id("i-0123456789abcdef0")
        .state(
            InstanceState::builder()
                .name(InstanceStateName::Running)
                .build(),
        )
        .private_ip_address("10.0.0.1")
        .public_ip_address("203.0.113.1")
        .tags(Tag::builder().key("telchar-pool").value("builders").build())
        .tags(Tag::builder().key("telchar-capacity").value("999").build())
        .build();
    let mut stopped = instance.clone();
    stopped.state = Some(
        InstanceState::builder()
            .name(InstanceStateName::Stopped)
            .build(),
    );
    let mut untagged = instance.clone();
    untagged.tags = None;
    let mut missing = instance.clone();
    missing.private_ip_address = None;
    let members = telchar::service::static_ssh_ec2::map_instances(
        &config.static_ssh_ec2()[0],
        &[instance, stopped, untagged, missing],
    )
    .unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].destination(), "telchar@10.0.0.1");
    assert_eq!(members[0].maximum_concurrent_builds(), 2);
    assert!(members[0].target().name().contains("i-0123456789abcdef0"));
}

#[test]
fn loads_ec2_membership_with_instance_role_credentials() {
    let root = tempfile::tempdir().unwrap();
    let path = configuration(
        root.path(),
        r#"
source = "ec2"
region = "us-east-1"
[backends.ssh.builders.tags]
telchar-pool = "builders"
"#,
    );
    let config =
        ServiceConfig::load_from_default(&path).expect("EC2 discovery configuration loads");
    let pool = &config.static_ssh_ec2()[0];
    assert_eq!(pool.region(), "us-east-1");
    assert_eq!(
        pool.address(),
        telchar::service::config::Ec2Address::PrivateIp
    );
    assert_eq!(pool.tags()["telchar-pool"], "builders");
    assert!(pool.credentials().is_none());
}
