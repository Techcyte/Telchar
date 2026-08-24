//! Tests Consul service membership mapping into ordinary static SSH backends.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use telchar::service::config::ServiceConfig;

fn discovery_config(root: &std::path::Path) -> telchar::service::config::StaticSshConsulConfig {
    let identity = root.join("identity");
    let known_hosts = root.join("known-hosts");
    let ssh = root.join("ssh");
    fs::write(&identity, "private").expect("identity writes");
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600))
        .expect("identity permissions set");
    fs::write(&known_hosts, "@cert-authority * ssh-ed25519 AAAA\n").expect("known hosts writes");
    fs::write(&ssh, "#!/bin/sh\nexit 1\n").expect("SSH writes");
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).expect("SSH permissions set");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        format!(
            r#"
[[backends.static_ssh_consul]]
name = "spot"
system = "x86_64-linux"
supported_features = ["ephemeral"]
maximum_concurrent_builds_per_instance = 2
endpoint = "http://127.0.0.1:8500"
service = "builder"
required_tags = ["ephemeral"]
refresh_interval_seconds = 15
request_timeout_seconds = 5
ssh_user = "telchar"
identity_file = "{}"
known_hosts_file = "{}"
ssh_program = "{}"
"#,
            identity.display(),
            known_hosts.display(),
            ssh.display(),
        ),
    )
    .expect("configuration writes");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load().expect("configuration loads");
    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    config.static_ssh_consul()[0].clone()
}

#[test]
fn maps_only_matching_healthy_members_with_service_address_fallback() {
    let root = tempfile::tempdir().expect("fixture creates");
    let config = discovery_config(root.path());
    let response = serde_json::json!([
        {
            "Node": {"Node": "node-a", "Address": "10.0.0.1"},
            "Service": {
                "ID": "builder-a",
                "Service": "builder",
                "Tags": ["ephemeral", "nix"],
                "Address": "10.0.1.1",
                "Port": 22
            },
            "Checks": [{"Status": "passing"}]
        },
        {
            "Node": {"Node": "node-b", "Address": "10.0.0.2"},
            "Service": {
                "ID": "builder-b",
                "Service": "builder",
                "Tags": ["ephemeral"],
                "Address": "",
                "Port": 2222
            },
            "Checks": [{"Status": "passing"}]
        },
        {
            "Node": {"Node": "node-c", "Address": "10.0.0.3"},
            "Service": {
                "ID": "builder-c",
                "Service": "builder",
                "Tags": ["ephemeral"],
                "Address": "10.0.1.3",
                "Port": 22
            },
            "Checks": [{"Status": "critical"}]
        },
        {
            "Node": {"Node": "node-d", "Address": "10.0.0.4"},
            "Service": {
                "ID": "builder-d",
                "Service": "builder",
                "Tags": ["other"],
                "Address": "10.0.1.4",
                "Port": 22
            },
            "Checks": [{"Status": "passing"}]
        }
    ]);

    let backends =
        telchar::service::static_ssh_consul::map_members(&config, &response).expect("members map");

    assert_eq!(backends.len(), 2);
    assert_eq!(backends[0].destination(), "telchar@10.0.1.1");
    assert_eq!(backends[0].port(), 22);
    assert_eq!(backends[1].destination(), "telchar@10.0.0.2");
    assert_eq!(backends[1].port(), 2222);
    assert_eq!(backends[0].maximum_concurrent_builds(), 2);
    assert_eq!(backends[0].target().features(), ["ephemeral"]);
    assert_ne!(backends[0].target().name(), backends[1].target().name());
}
