//! Tests optional Consul-backed static SSH discovery configuration.

use super::*;

#[test]
fn loads_optional_consul_static_ssh_discovery_without_static_inventory() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("static-ssh-consul");
    let identity_file = root.join("builder-key");
    let known_hosts_file = root.join("known-hosts");
    let token_file = root.join("consul-token");
    let ca_file = root.join("consul-ca.pem");
    let ssh_program = root.join("ssh");
    fs::write(&identity_file, "private-key").expect("identity writes");
    fs::set_permissions(&identity_file, fs::Permissions::from_mode(0o600))
        .expect("identity permissions set");
    fs::write(&known_hosts_file, "@cert-authority * ssh-ed25519 AAAA\n")
        .expect("known hosts writes");
    fs::write(&token_file, "consul-token\n").expect("token writes");
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600))
        .expect("token permissions set");
    fs::write(&ca_file, "certificate\n").expect("CA writes");
    fs::set_permissions(&ca_file, fs::Permissions::from_mode(0o644)).expect("CA permissions set");
    fs::write(&ssh_program, "#!/bin/sh\nexit 1\n").expect("SSH program writes");
    fs::set_permissions(&ssh_program, fs::Permissions::from_mode(0o755))
        .expect("SSH program permissions set");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        format!(
            r#"
[[backends.ssh]]
system = "x86_64-linux"
supported_features = ["ephemeral"]
maximum_concurrent_builds = 2
selection_priority = 6
ssh_user = "telchar"
identity_file = "{}"
known_hosts_file = "{}"
ssh_program = "{}"

[backends.ssh.spot-builders]
source = "consul"
endpoint = "https://consul.example:8501"
service = "telchar-ssh-builder"
datacenter = "dc1"
required_tags = ["ephemeral", "nix"]
refresh_interval_seconds = 15
request_timeout_seconds = 5
token_file = "{}"
ca_certificate_file = "{}"
"#,
            identity_file.display(),
            known_hosts_file.display(),
            ssh_program.display(),
            token_file.display(),
            ca_file.display(),
        ),
    )
    .expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };

    let config = ServiceConfig::load().expect("configuration loads");
    assert!(config.static_ssh_backends().is_empty());
    let discovery = &config.static_ssh_consul()[0];
    assert_eq!(discovery.name(), "spot-builders");
    assert_eq!(discovery.system(), "x86_64-linux");
    assert_eq!(discovery.supported_features(), ["ephemeral"]);
    assert_eq!(discovery.maximum_concurrent_builds_per_instance(), 2);
    assert_eq!(discovery.selection_priority(), 6);
    assert_eq!(discovery.endpoint(), "https://consul.example:8501");
    assert_eq!(discovery.service(), "telchar-ssh-builder");
    assert_eq!(discovery.datacenter(), Some("dc1"));
    assert_eq!(discovery.required_tags(), ["ephemeral", "nix"]);
    assert!(discovery.passing_only());
    assert_eq!(discovery.refresh_interval().as_secs(), 15);
    assert_eq!(discovery.request_timeout().as_secs(), 5);
    assert_eq!(discovery.ssh_user(), "telchar");

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}
