//! Tests Consul service membership mapping into ordinary static SSH backends.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use telchar::service::config::ServiceConfig;

fn discovery_config(
    root: &std::path::Path,
    endpoint: &str,
) -> telchar::service::config::StaticSshConsulConfig {
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
[[backends.ssh]]
system = "x86_64-linux"
maximum_concurrent_builds = 1
ssh_user = "telchar"
identity_file = "{}"
known_hosts_file = "{}"
ssh_program = "{}"

[backends.ssh.manual]
source = "static"

[backends.ssh.manual.builder]
address = "manual.example"

[backends.ssh.spot]
source = "consul"
supported_features = ["ephemeral"]
maximum_concurrent_builds = 2
endpoint = "{endpoint}"
service = "builder"
required_tags = ["ephemeral"]
refresh_interval_seconds = 15
request_timeout_seconds = 5
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
fn merges_discovered_members_with_manual_inventory_without_replacing_it() {
    let root = tempfile::tempdir().expect("fixture creates");
    let config_path = root.path().join("telchar.toml");
    let discovery = discovery_config(root.path(), "http://127.0.0.1:8500");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load().expect("configuration loads");
    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    let discovered = map_fixture_members(&discovery);

    let merged = telchar::service::static_ssh_consul::merge_inventory(&config, &discovered)
        .expect("inventory merges");

    assert_eq!(merged.len(), 2);
    assert!(
        merged
            .iter()
            .any(|backend| backend.target().name() == "manual.builder")
    );
    assert!(
        merged
            .iter()
            .any(|backend| backend.target().name().starts_with("spot-"))
    );
}

#[test]
fn publishes_merged_inventory_as_a_reloadable_backend_generation() {
    let root = tempfile::tempdir().expect("fixture creates");
    let config_path = root.path().join("telchar.toml");
    let discovery = discovery_config(root.path(), "http://127.0.0.1:8500");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load().expect("configuration loads");
    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    let initial = telchar::backend::routing::ConfiguredBackends::with_health(
        &config,
        None,
        None,
        telchar::backend::static_ssh::StaticSshHealth::from_states(
            config.static_ssh_backends(),
            [(
                "manual.builder",
                telchar::backend::static_ssh::StaticSshHealthState::Ready,
            )],
        ),
    )
    .expect("initial backends configure");
    let reloadable = telchar::backend::routing::ReloadableBackends::new(initial);
    let discovered = map_fixture_members(&discovery);

    telchar::service::static_ssh_consul::publish_inventory(
        &config,
        &discovered,
        &reloadable,
        None,
        None,
    )
    .expect("inventory publishes");

    let snapshot = reloadable.snapshot();
    assert!(
        snapshot
            .targets()
            .any(|target| target.name() == "manual.builder")
    );
    assert!(
        snapshot
            .targets()
            .any(|target| target.name().starts_with("spot-"))
    );
}

#[test]
fn refresh_preserves_last_successful_inventory_after_transient_failure() {
    let root = tempfile::tempdir().expect("fixture creates");
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let endpoint = format!("http://{}", listener.local_addr().expect("address reads"));
    let config = discovery_config(root.path(), &endpoint);
    let server = std::thread::spawn(move || {
        let (mut first, _) = listener.accept().expect("first request accepts");
        let mut request_buffer = [0_u8; 4096];
        let count = first
            .read(&mut request_buffer)
            .expect("first request reads");
        let request = std::str::from_utf8(&request_buffer[..count]).expect("request is UTF-8");
        assert!(request.starts_with("GET /v1/health/service/builder?"));
        assert!(request.contains("passing=true"));
        assert!(request.contains("tag=ephemeral"));
        let body = serde_json::json!([{
            "Node": {"Node": "node-a", "Address": "10.0.0.1"},
            "Service": {
                "ID": "builder-a", "Service": "builder", "Tags": ["ephemeral"],
                "Address": "10.0.1.1", "Port": 22
            },
            "Checks": [{"Status": "passing"}]
        }])
        .to_string();
        write!(
            first,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("first response writes");

        let (mut second, _) = listener.accept().expect("second request accepts");
        let _ = second
            .read(&mut request_buffer)
            .expect("second request reads");
        write!(
            second,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .expect("second response writes");
    });
    let inventory = Arc::new(RwLock::new(Vec::new()));
    let mut discovery = telchar::service::static_ssh_consul::ConsulSshDiscovery::new(
        config,
        Arc::clone(&inventory),
    )
    .expect("discovery creates");

    assert_eq!(discovery.refresh().expect("first refresh succeeds"), 1);
    let retained = inventory.read().expect("inventory reads")[0]
        .target()
        .name()
        .to_owned();
    assert!(discovery.refresh().is_err());
    assert_eq!(inventory.read().expect("inventory reads").len(), 1);
    assert_eq!(
        inventory.read().expect("inventory reads")[0]
            .target()
            .name(),
        retained
    );
    server.join().expect("server joins");
}

#[test]
fn background_service_survives_transient_failure_and_reports_later_inventory() {
    let root = tempfile::tempdir().expect("fixture creates");
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let endpoint = format!("http://{}", listener.local_addr().expect("address reads"));
    let config = discovery_config(root.path(), &endpoint);
    let server = std::thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        let (mut failed, _) = listener.accept().expect("failed request accepts");
        let _ = failed.read(&mut buffer).expect("failed request reads");
        write!(
            failed,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .expect("failed response writes");

        let (mut successful, _) = listener.accept().expect("successful request accepts");
        let _ = successful
            .read(&mut buffer)
            .expect("successful request reads");
        let body = serde_json::json!([{
            "Node": {"Node": "node-a", "Address": "10.0.0.1"},
            "Service": {
                "ID": "builder-a", "Service": "builder", "Tags": ["ephemeral"],
                "Address": "10.0.1.1", "Port": 22
            },
            "Checks": [{"Status": "passing"}]
        }])
        .to_string();
        write!(
            successful,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("successful response writes");
    });
    let mut service = telchar::service::static_ssh_consul::ConsulSshDiscoveryService::start(
        vec![config],
        Duration::from_millis(10),
    )
    .expect("service starts");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let inventory = loop {
        if let Some(inventory) = service.check().expect("service checks") {
            break inventory;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "inventory was not reported"
        );
        std::thread::sleep(Duration::from_millis(10));
    };

    assert_eq!(inventory.len(), 1);
    service.shutdown().expect("service shuts down");
    server.join().expect("server joins");
}

#[test]
fn successful_empty_refresh_removes_only_discovered_inventory() {
    let root = tempfile::tempdir().expect("fixture creates");
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let endpoint = format!("http://{}", listener.local_addr().expect("address reads"));
    let config = discovery_config(root.path(), &endpoint);
    let initial = map_fixture_members(&config);
    let inventory = Arc::new(RwLock::new(initial));
    let server = std::thread::spawn(move || {
        let (mut request, _) = listener.accept().expect("request accepts");
        let mut buffer = [0_u8; 4096];
        let _ = request.read(&mut buffer).expect("request reads");
        write!(
            request,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]"
        )
        .expect("response writes");
    });
    let mut discovery = telchar::service::static_ssh_consul::ConsulSshDiscovery::new(
        config,
        Arc::clone(&inventory),
    )
    .expect("discovery creates");

    assert_eq!(discovery.refresh().expect("empty refresh succeeds"), 0);
    assert!(inventory.read().expect("inventory reads").is_empty());
    server.join().expect("server joins");
}

fn map_fixture_members(
    config: &telchar::service::config::StaticSshConsulConfig,
) -> Vec<telchar::service::config::StaticSshBackendConfig> {
    telchar::service::static_ssh_consul::map_members(
        config,
        &serde_json::json!([{
            "Node": {"Node": "node-a", "Address": "10.0.0.1"},
            "Service": {
                "ID": "builder-a", "Service": "builder", "Tags": ["ephemeral"],
                "Address": "10.0.1.1", "Port": 22
            },
            "Checks": [{"Status": "passing"}]
        }]),
    )
    .expect("fixture members map")
}

#[test]
fn service_metadata_overrides_only_approved_backend_properties() {
    let root = tempfile::tempdir().expect("fixture creates");
    let config = discovery_config(root.path(), "http://127.0.0.1:8500");
    let response = serde_json::json!([{
        "Node": {"Node": "node-a", "Address": "10.0.0.1", "Meta": {
            "telchar_system": "ignored-node-system"
        }},
        "Service": {
            "ID": "builder-a", "Service": "builder", "Tags": ["ephemeral"],
            "Address": "10.0.1.1", "Port": 22,
            "Meta": {
                "telchar_system": "aarch64-linux",
                "telchar_supported_features": "kvm,big-parallel",
                "telchar_mandatory_features": "kvm",
                "telchar_maximum_concurrent_builds": "4",
                "telchar_ssh_user": "attacker"
            }
        },
        "Checks": [{"Status": "passing"}]
    }]);

    let backends =
        telchar::service::static_ssh_consul::map_members(&config, &response).expect("members map");

    assert_eq!(backends[0].target().system(), "aarch64-linux");
    assert_eq!(backends[0].target().features(), ["kvm", "big-parallel"]);
    assert_eq!(backends[0].target().mandatory_features(), ["kvm"]);
    assert_eq!(backends[0].maximum_concurrent_builds(), 4);
    assert_eq!(backends[0].destination(), "telchar@10.0.1.1");
}

#[test]
fn maps_only_matching_healthy_members_with_service_address_fallback() {
    let root = tempfile::tempdir().expect("fixture creates");
    let config = discovery_config(root.path(), "http://127.0.0.1:8500");
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
