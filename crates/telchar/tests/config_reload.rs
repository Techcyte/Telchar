//! Tests transactional additive static SSH backend reload generations.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use telchar::backend::routing::{ConfiguredBackends, ReloadableBackends};
use telchar::backend::static_ssh::{StaticSshHealth, StaticSshHealthState};
use telchar::backend::{BuildBackend, BuildExecution};
use telchar::service::config::ServiceConfig;
use telchar::service::config_reload::BackendReload;
use telchar::service::daemon_services::StaticSshHealthService;

#[path = "support/build_request.rs"]
mod build_request_support;

use build_request_support::admitted_request;

static ENVIRONMENT: Mutex<()> = Mutex::new(());

#[test]
fn reload_publishes_inventory_generation_and_disables_removed_hosts_in_old_snapshots() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let root = tempfile::tempdir().expect("fixture creates");
    let identity = root.path().join("identity");
    let known_hosts = root.path().join("known-hosts");
    let ssh = root.path().join("ssh");
    fs::write(&identity, "identity").expect("identity writes");
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600))
        .expect("identity permissions set");
    fs::write(&known_hosts, "builder ssh-ed25519 AAAA\n").expect("known hosts writes");
    fs::write(
        &ssh,
        "#!/bin/sh\nexec 3>&-\nprintf '\\x01\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x13\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00'\n",
    )
    .expect("SSH program writes");
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).expect("SSH permissions set");
    let config_path = root.path().join("telchar.toml");
    let backend = |name: &str| {
        format!(
            "[[backends.static_ssh]]\nname = \"{name}\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\ndestination = \"{name}\"\nidentity_file = \"{}\"\nknown_hosts_file = \"{}\"\nssh_program = \"{}\"\n",
            identity.display(),
            known_hosts.display(),
            ssh.display()
        )
    };
    let saved = std::env::var_os("TELCHAR_CONFIG");
    fs::write(
        &config_path,
        format!("{}{}", backend("builder-a"), backend("builder-b")),
    )
    .expect("initial configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let mut current = ServiceConfig::load().expect("initial configuration loads");
    let health = StaticSshHealth::from_states(
        current.static_ssh_backends(),
        [
            ("builder-a", StaticSshHealthState::Ready),
            ("builder-b", StaticSshHealthState::Ready),
        ],
    );
    let initial = ConfiguredBackends::with_health(&current, None, None, health.clone())
        .expect("initial backends configure");
    let old_snapshot = initial.clone();
    let mut old_executor = old_snapshot
        .executor("postgresql://fixture")
        .expect("old executor configures");
    let selected = old_executor
        .selected_target("x86_64-linux", &[])
        .expect("old generation selects first backend");
    assert_eq!(selected.name(), "builder-a");
    let reloadable = ReloadableBackends::new(initial);
    let mut health_service = StaticSshHealthService::start(health, Duration::from_secs(60))
        .expect("health service starts");

    fs::write(
        &config_path,
        format!("{}{}", backend("builder-b"), backend("builder-c")),
    )
    .expect("replacement configuration writes");
    let replacement = ServiceConfig::load().expect("replacement configuration loads");
    let reload =
        BackendReload::prepare_config(&current, replacement, None, None, Duration::from_secs(60))
            .expect("reload prepares");
    assert_eq!(
        reload
            .apply(&mut current, &reloadable, &mut health_service)
            .expect("reload applies"),
        telchar::service::config::StaticSshReloadChanges {
            added: 1,
            removed: 1,
        }
    );

    assert_eq!(old_snapshot.static_ssh_health().state("builder-c"), None);
    assert!(old_executor.selected_target("x86_64-linux", &[]).is_ok());
    let build = admitted_request();
    let mut execution =
        BuildExecution::new("assigned-before-reload", &build, Duration::from_secs(1))
            .expect("execution constructs");
    execution
        .set_target_name("builder-a")
        .expect("exact target records");
    assert!(old_executor.execute(&execution).is_err());
    assert!(!old_snapshot
        .static_ssh_scheduling()
        .read()
        .expect("scheduling reads")
        .contains("builder-a"));
    assert_eq!(
        reloadable.snapshot().static_ssh_health().state("builder-c"),
        Some(StaticSshHealthState::Unavailable)
    );
    assert_eq!(current.static_ssh_backends().len(), 2);
    health_service
        .shutdown()
        .expect("health service shuts down");
    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
}

#[test]
fn reload_refreshes_nomad_token_file_contents() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let root = tempfile::tempdir().expect("fixture creates");
    let token_file = root.path().join("nomad.token");
    fs::write(&token_file, "first-token\n").expect("initial token writes");
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600))
        .expect("token permissions set");
    let listener = TcpListener::bind("127.0.0.1:0").expect("Nomad fixture binds");
    let endpoint = format!("http://{}", listener.local_addr().expect("address reads"));
    let config_path = root.path().join("telchar.toml");
    fs::write(&config_path, nomad_config(&endpoint, &token_file)).expect("configuration writes");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
        .expect("configuration permissions set");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let mut current = ServiceConfig::load().expect("initial configuration loads");
    let initial = ConfiguredBackends::new(&current, None).expect("initial backends configure");
    let reloadable = ReloadableBackends::new(initial);
    let health = StaticSshHealth::from_states(&[], []);
    let mut health_service = StaticSshHealthService::start(health, Duration::from_secs(60))
        .expect("health service starts");

    fs::write(&token_file, "replacement-token\n").expect("replacement token writes");
    let replacement = ServiceConfig::load().expect("replacement configuration loads");
    BackendReload::prepare_config(&current, replacement, None, None, Duration::from_secs(60))
        .expect("reload prepares")
        .apply(&mut current, &reloadable, &mut health_service)
        .expect("reload applies");

    let server = thread::spawn(move || {
        let (mut request, _) = listener.accept().expect("Nomad request accepts");
        request
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout sets");
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = request.read(&mut buffer).expect("request reads");
            assert!(count > 0, "request ended before headers");
            bytes.extend_from_slice(&buffer[..count]);
            if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                break;
            }
        }
        let request_text = String::from_utf8(bytes).expect("request is UTF-8");
        assert!(request_text
            .to_ascii_lowercase()
            .contains("x-nomad-token: replacement-token\r\n"));
        write!(
            request,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .expect("response writes");
    });
    let client = telchar::nomad::backend::NomadClient::new(current.nomad_backends()[0].clone())
        .expect("Nomad client configures");
    assert_eq!(
        client
            .status("missing-job")
            .expect("missing Nomad job maps to terminal state"),
        telchar::nomad::backend::NomadExecutionState::Missing
    );
    server.join().expect("Nomad fixture joins");
    health_service
        .shutdown()
        .expect("health service shuts down");
    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
}

fn nomad_config(endpoint: &str, token_file: &std::path::Path) -> String {
    format!(
        r#"
[[backends.nomad]]
name = "nomad-test"
system = "x86_64-linux"
maximum_concurrent_builds = 1
endpoint = "{endpoint}"
namespace = "telchar"
token_file = {token_file:?}
driver = "raw_exec"
job_name_scope = "telchar-test"
poll_interval_seconds = 1
runtime_limit_seconds = 60
transfer_endpoint = "ws://telchar.example:7443"

[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "{endpoint}"
jwks_url = "{endpoint}/.well-known/jwks.json"
audience = "telchar-transfer"

[backends.nomad.store]
mode = "daemon"
uri = "unix:///nix/var/nix/daemon-socket/socket"

[backends.nomad.transfer_limits]
maximum_manifest_paths = 1024
maximum_manifest_bytes = 1048576
maximum_input_nar_bytes = 1073741824
maximum_total_input_bytes = 8589934592
maximum_output_nar_bytes = 1073741824
maximum_total_output_bytes = 8589934592
maximum_frame_metadata_bytes = 65536
stream_buffer_bytes = 262144
maximum_live_log_chunk_bytes = 65536
live_log_queue_bytes = 1048576
transfer_idle_timeout_seconds = 30
setup_timeout_seconds = 300
output_collection_timeout_seconds = 300
maximum_connection_lifetime_seconds = 3600
authentication_lifetime_seconds = 300
clock_skew_seconds = 30
nonce_retention_seconds = 600
reconnect_timeout_seconds = 30
maximum_diagnostic_bytes = 65536

[backends.nomad.resources]
cpu_mhz = 1000
memory_mb = 512
disk_mb = 1024

[backends.nomad.driver_config]
command = "/bin/true"
"#
    )
}
