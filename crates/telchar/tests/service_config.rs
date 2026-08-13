use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use telchar::backend::BackendKind;
use telchar::config::ServiceConfig;

static ENVIRONMENT: Mutex<()> = Mutex::new(());
const VARIABLES: &[&str] = &[
    "TELCHAR_CONFIG",
    "TELCHAR_DATABASE_URL",
    "TELCHAR_RUNNING_DISCONNECT_POLICY",
    "TELCHAR_OUTPUT_RETENTION_SECONDS",
    "TELCHAR_MAX_RETAINED_INPUT_BYTES",
    "TELCHAR_IPC_SOCKET",
    "TELCHAR_IPC_MAX_SESSIONS",
    "TELCHAR_IDENTITY_MAPPINGS_FILE",
];

#[test]
fn loads_strict_toml_and_identity_mappings() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("toml");
    let database_url_file = root.join("database-url");
    fs::write(
        &database_url_file,
        "postgresql://telchar@localhost/telchar\n",
    )
    .expect("database URL writes");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        format!(
            r#"
running_disconnect_policy = "cancel-running"
output_retention_seconds = 7200
maximum_retained_input_bytes = 1048576

[database]
url_file = "{}"

[ipc]
socket = "/run/telchar/daemon.sock"
maximum_sessions = 32

[identity.credentials."ssh-pubkey:SHA256:abc"]
audit_subject = "travis"
quota_subject = "engineering"

[identity.credentials."ssh-pubkey:SHA256:def"]
audit_subject = "automation"
quota_subject = "engineering"

[scheduling.default]
maximum_queued_builds = 20
maximum_active_builds = 4

[scheduling.subjects.release-engineering]
maximum_queued_builds = 100
maximum_active_builds = 16

[backends]
permit_wait_seconds = 30

[backends.local]
name = "local"
system = "x86_64-linux"
supported_features = ["kvm"]
maximum_concurrent_builds = 2
"#,
            database_url_file.display()
        ),
    )
    .expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };

    let config = ServiceConfig::load().expect("configuration loads");

    assert_eq!(config.backend_targets().count(), 1);
    assert_eq!(
        config.running_disconnect_policy(),
        telchar::deployment::RunningDisconnectPolicy::CancelRunning
    );
    assert_eq!(config.output_retention().seconds(), 7200);
    assert_eq!(config.maximum_retained_input_bytes(), 1_048_576);
    assert_eq!(
        config.database_url().expect("database configured"),
        "postgresql://telchar@localhost/telchar"
    );
    assert_eq!(
        config.ipc_socket().expect("IPC socket configured"),
        Path::new("/run/telchar/daemon.sock")
    );
    assert_eq!(config.maximum_ipc_sessions(), 32);
    assert_eq!(config.nomad_callback().bind().to_string(), "0.0.0.0:7443");
    assert_eq!(
        config.nomad_callback().public_url(),
        "ws://127.0.0.1:7443/callback"
    );
    assert_eq!(config.nomad_callback().maximum_connections(), 64);
    assert_eq!(config.nomad_callback().maximum_header_bytes(), 16 * 1024);
    assert_eq!(config.nomad_callback().maximum_body_bytes(), 64 * 1024);
    assert_eq!(
        config
            .nomad_callback()
            .authentication_request_timeout()
            .as_secs(),
        10
    );
    assert_eq!(config.nomad_callback().maximum_jwks_bytes(), 1024 * 1024);
    assert_eq!(config.nomad_callback().maximum_retained_nonces(), 65_536);
    assert_eq!(config.backend_permit_wait().as_secs(), 30);
    assert_eq!(
        config.scheduling_limits("unknown-subject"),
        telchar::config::SchedulingLimits::new(20, 4).expect("limits are valid")
    );
    assert_eq!(
        config.scheduling_limits("release-engineering"),
        telchar::config::SchedulingLimits::new(100, 16).expect("limits are valid")
    );
    let local = config.local_backend().expect("local backend exists");
    assert_eq!(local.target().name(), "local");
    assert_eq!(local.maximum_concurrent_builds(), 2);
    let mapping = config
        .credential_mapping("ssh-pubkey:SHA256:abc")
        .expect("credential mapping exists");
    assert_eq!(mapping.audit_subject.as_deref(), Some("travis"));
    assert_eq!(mapping.quota_subject.as_deref(), Some("engineering"));
    let second_mapping = config
        .credential_mapping("ssh-pubkey:SHA256:def")
        .expect("second credential mapping exists");
    assert_eq!(second_mapping.audit_subject.as_deref(), Some("automation"));
    assert_eq!(second_mapping.quota_subject.as_deref(), Some("engineering"));

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn loads_configured_nomad_callback_service() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("nomad-callback");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        r#"
[nomad_callback]
bind = "127.0.0.1:17443"
public_url = "wss://gateway.internal/build-callback"
maximum_connections = 12
maximum_header_bytes = 8192
maximum_body_bytes = 32768
authentication_request_timeout_seconds = 7
maximum_jwks_bytes = 131072
maximum_retained_nonces = 4096
"#,
    )
    .expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };

    let config = ServiceConfig::load().expect("configuration loads");
    let callback = config.nomad_callback();
    assert_eq!(callback.bind().to_string(), "127.0.0.1:17443");
    assert_eq!(
        callback.public_url(),
        "wss://gateway.internal/build-callback"
    );
    assert_eq!(callback.maximum_connections(), 12);
    assert_eq!(callback.maximum_header_bytes(), 8192);
    assert_eq!(callback.maximum_body_bytes(), 32768);
    assert_eq!(callback.authentication_request_timeout().as_secs(), 7);
    assert_eq!(callback.maximum_jwks_bytes(), 131072);
    assert_eq!(callback.maximum_retained_nonces(), 4096);

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn nomad_backend_uses_callback_public_url_when_endpoint_is_omitted() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("nomad-callback-default");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        r#"
[nomad_callback]
public_url = "ws://gateway.internal:17443/build-callback"

[[backends.nomad]]
name = "nomad-primary"
system = "x86_64-linux"
supported_features = []
maximum_concurrent_builds = 1
endpoint = "http://nomad.internal:4646"
namespace = "telchar"
driver = "raw_exec"
job_name_scope = "telchar"
poll_interval_seconds = 1
runtime_limit_seconds = 60

[backends.nomad.driver_config]
command = "/bin/true"

[backends.nomad.resources]
cpu_mhz = 100
memory_mb = 128
disk_mb = 128

[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "http://nomad.internal:4646"
jwks_url = "http://nomad.internal:4646/.well-known/jwks.json"
audience = "telchar"

[backends.nomad.store]
mode = "daemon"
uri = "daemon"

[backends.nomad.transfer_limits]
maximum_manifest_paths = 100
maximum_manifest_bytes = 1048576
maximum_input_nar_bytes = 1048576
maximum_total_input_bytes = 10485760
maximum_output_nar_bytes = 1048576
maximum_total_output_bytes = 10485760
maximum_frame_metadata_bytes = 65536
stream_buffer_bytes = 65536
maximum_live_log_chunk_bytes = 16384
live_log_queue_bytes = 1048576
transfer_idle_timeout_seconds = 60
setup_timeout_seconds = 60
output_collection_timeout_seconds = 60
authentication_lifetime_seconds = 60
clock_skew_seconds = 5
nonce_retention_seconds = 120
reconnect_timeout_seconds = 60
maximum_diagnostic_bytes = 65536
"#,
    )
    .expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };

    let config = ServiceConfig::load().expect("configuration loads");
    assert_eq!(
        config.nomad_backends()[0].transfer_endpoint(),
        "ws://gateway.internal:17443/build-callback"
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn rejects_invalid_nomad_callback_service_configuration() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("invalid-nomad-callback");
    let config_path = root.join("telchar.toml");

    for callback in [
        r#"bind = "gateway.example:7443""#,
        r#"public_url = "http://gateway.example/callback""#,
        "maximum_connections = 0",
        "maximum_header_bytes = 0",
        "maximum_body_bytes = 0",
        "authentication_request_timeout_seconds = 0",
        "maximum_jwks_bytes = 0",
        "maximum_retained_nonces = 0",
    ] {
        fs::write(&config_path, format!("[nomad_callback]\n{callback}\n"))
            .expect("configuration writes");
        unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
        assert!(ServiceConfig::load().is_err(), "accepted {callback}");
    }

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn loads_static_ssh_backend_with_fixed_credentials_and_pinned_host_keys() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("static-ssh");
    let identity_file = root.join("builder-key");
    let known_hosts_file = root.join("known-hosts");
    let ssh_program = root.join("ssh");
    fs::write(&identity_file, "private-key").expect("identity writes");
    fs::set_permissions(&identity_file, fs::Permissions::from_mode(0o600))
        .expect("identity permissions set");
    fs::write(&known_hosts_file, "builder.example ssh-ed25519 AAAA\n").expect("known hosts writes");
    fs::write(&ssh_program, "#!/bin/sh\nexit 1\n").expect("SSH program writes");
    fs::set_permissions(&ssh_program, fs::Permissions::from_mode(0o755))
        .expect("SSH program permissions set");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        format!(
            r#"
[[backends.static_ssh]]
name = "darwin-builder"
system = "aarch64-darwin"
supported_features = ["apple-virt", "big-parallel"]
maximum_concurrent_builds = 4
destination = "telchar-builder@builder.example"
identity_file = "{}"
known_hosts_file = "{}"
ssh_program = "{}"
"#,
            identity_file.display(),
            known_hosts_file.display(),
            ssh_program.display()
        ),
    )
    .expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };

    let config = ServiceConfig::load().expect("configuration loads");
    let backend = config
        .static_ssh_backends()
        .first()
        .expect("static SSH backend exists");

    assert_eq!(backend.target().name(), "darwin-builder");
    assert_eq!(backend.target().kind(), BackendKind::StaticSsh);
    assert_eq!(backend.target().system(), "aarch64-darwin");
    assert_eq!(backend.target().features(), ["apple-virt", "big-parallel"]);
    assert_eq!(backend.maximum_concurrent_builds(), 4);
    assert_eq!(backend.destination(), "telchar-builder@builder.example");
    assert_eq!(backend.identity_file(), identity_file);
    assert_eq!(backend.known_hosts_file(), known_hosts_file);
    assert_eq!(backend.ssh_program(), ssh_program);

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn loads_fungible_nomad_backends_with_operator_controlled_drivers() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("nomad");
    let token_file = root.join("nomad-token");
    let ca_certificate_file = root.join("nomad-ca.pem");
    fs::write(&token_file, "secret-token\n").expect("token writes");
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600))
        .expect("token permissions set");
    fs::write(&ca_certificate_file, "certificate\n").expect("CA certificate writes");
    fs::set_permissions(&ca_certificate_file, fs::Permissions::from_mode(0o644))
        .expect("CA certificate permissions set");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        format!(
            r#"
[[backends.nomad]]
name = "nomad-docker"
system = "x86_64-linux"
supported_features = ["docker"]
maximum_concurrent_builds = 8
endpoint = "https://nomad-a.example:4646"
namespace = "telchar-a"
token_file = "{}"
ca_certificate_file = "{}"
driver = "docker"
job_name_scope = "prod-a"
poll_interval_seconds = 2
runtime_limit_seconds = 3600

transfer_endpoint = "ws://telchar.example:7443"

[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "http://nomad.example:4646"
jwks_url = "http://nomad.example:4646/.well-known/jwks.json"
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
authentication_lifetime_seconds = 300
clock_skew_seconds = 30
nonce_retention_seconds = 600
reconnect_timeout_seconds = 30
maximum_diagnostic_bytes = 65536

[backends.nomad.resources]
cpu_mhz = 2000
memory_mb = 4096
disk_mb = 16384

[backends.nomad.driver_config]
image = "registry.example/telchar-builder:1"
privileged = false

[[backends.nomad]]
name = "nomad-raw"
system = "aarch64-linux"
supported_features = ["raw-exec", "big-parallel"]
maximum_concurrent_builds = 2
endpoint = "http://nomad-b.example:4646"
namespace = "telchar-b"
driver = "raw_exec"
job_name_scope = "prod-b"
poll_interval_seconds = 5
runtime_limit_seconds = 1800

transfer_endpoint = "ws://telchar.example:7443"

[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "http://nomad.example:4646"
jwks_url = "http://nomad.example:4646/.well-known/jwks.json"
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
authentication_lifetime_seconds = 300
clock_skew_seconds = 30
nonce_retention_seconds = 600
reconnect_timeout_seconds = 30
maximum_diagnostic_bytes = 65536

[backends.nomad.resources]
cpu_mhz = 1000
memory_mb = 2048
disk_mb = 8192

[backends.nomad.driver_config]
command = "/opt/telchar/bin/nomad-worker"
args = ["--stdio"]
"#,
            token_file.display(),
            ca_certificate_file.display()
        ),
    )
    .expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };

    let config = ServiceConfig::load().expect("configuration loads");
    let backends = config.nomad_backends();
    assert_eq!(backends.len(), 2);
    assert_eq!(backends[0].target().name(), "nomad-docker");
    assert_eq!(backends[0].target().kind(), BackendKind::Nomad);
    assert_eq!(backends[0].endpoint(), "https://nomad-a.example:4646");
    assert_eq!(backends[0].namespace(), "telchar-a");
    assert_eq!(backends[0].token_file(), Some(token_file.as_path()));
    assert_eq!(
        backends[0].ca_certificate_file(),
        Some(ca_certificate_file.as_path())
    );
    assert_eq!(backends[0].driver(), "docker");
    assert_eq!(backends[0].job_name_scope(), "prod-a");
    assert_eq!(backends[0].poll_interval().as_secs(), 2);
    assert_eq!(backends[0].runtime_limit().as_secs(), 3600);
    assert_eq!(backends[0].resources().cpu_mhz(), 2000);
    assert_eq!(backends[0].resources().memory_mb(), 4096);
    assert_eq!(backends[0].resources().disk_mb(), 16384);
    assert_eq!(
        backends[0].driver_config()["image"],
        "registry.example/telchar-builder:1"
    );
    assert_eq!(backends[0].driver_config()["privileged"], false);
    assert_eq!(backends[1].target().name(), "nomad-raw");
    assert_eq!(backends[1].driver(), "raw_exec");
    assert_eq!(
        backends[1].driver_config()["command"],
        "/opt/telchar/bin/nomad-worker"
    );
    assert_eq!(backends[1].driver_config()["args"][0], "--stdio");
    assert_eq!(
        config
            .backend_targets()
            .map(|target| target.name())
            .collect::<Vec<_>>(),
        ["nomad-docker", "nomad-raw"]
    );
    assert_eq!(
        config
            .system_features()
            .into_iter()
            .map(|(system, features)| (system, features.into_iter().collect::<Vec<_>>()))
            .collect::<Vec<_>>(),
        [
            ("aarch64-linux", vec!["big-parallel", "raw-exec"]),
            ("x86_64-linux", vec!["docker"]),
        ]
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn loads_nomad_transfer_configuration() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("nomad-transfer");
    let workload_ca_file = root.join("workload-ca.pem");
    fs::write(&workload_ca_file, "certificate\n").expect("workload CA writes");
    fs::set_permissions(&workload_ca_file, fs::Permissions::from_mode(0o644))
        .expect("workload CA permissions set");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        format!(
            r#"
[[backends.nomad]]
name = "nomad"
system = "x86_64-linux"
maximum_concurrent_builds = 2
endpoint = "https://nomad.example:4646"
namespace = "telchar"
driver = "raw_exec"
job_name_scope = "prod"
poll_interval_seconds = 2
runtime_limit_seconds = 3600
transfer_endpoint = "wss://telchar.example:7443"

[backends.nomad.resources]
cpu_mhz = 1000
memory_mb = 2048
disk_mb = 8192

[backends.nomad.driver_config]
command = "/opt/telchar/bin/telchar-nomad-worker"

[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "https://nomad.example:4646"
jwks_url = "https://nomad.example:4646/.well-known/jwks.json"
audience = "telchar-transfer"
ca_certificate_file = "{}"

[backends.nomad.store]
mode = "daemon"
uri = "unix:///nix/var/nix/daemon-socket/socket"

[backends.nomad.transfer_limits]
maximum_manifest_paths = 65536
maximum_manifest_bytes = 8388608
maximum_input_nar_bytes = 8589934592
maximum_total_input_bytes = 68719476736
maximum_output_nar_bytes = 8589934592
maximum_total_output_bytes = 68719476736
maximum_frame_metadata_bytes = 65536
stream_buffer_bytes = 262144
maximum_live_log_chunk_bytes = 65536
live_log_queue_bytes = 1048576
transfer_idle_timeout_seconds = 30
setup_timeout_seconds = 300
output_collection_timeout_seconds = 300
authentication_lifetime_seconds = 300
clock_skew_seconds = 30
nonce_retention_seconds = 600
reconnect_timeout_seconds = 30
maximum_diagnostic_bytes = 65536

[backends.nomad.prestart]
driver = "raw_exec"
timeout_seconds = 120

[backends.nomad.prestart.resources]
cpu_mhz = 100
memory_mb = 128
disk_mb = 256

[backends.nomad.prestart.driver_config]
command = "/opt/operator/bin/configure-nix"
args = ["/alloc/data/nix"]
"#,
            workload_ca_file.display()
        ),
    )
    .expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };

    let config = ServiceConfig::load().expect("configuration loads");
    let backend = &config.nomad_backends()[0];
    assert_eq!(backend.transfer_endpoint(), "wss://telchar.example:7443");
    let authentication = backend.transfer_authentication();
    assert_eq!(authentication.mode(), "workload-identity");
    assert_eq!(authentication.issuer(), Some("https://nomad.example:4646"));
    assert_eq!(
        authentication.jwks_url(),
        Some("https://nomad.example:4646/.well-known/jwks.json")
    );
    assert_eq!(authentication.audience(), Some("telchar-transfer"));
    assert_eq!(
        authentication.ca_certificate_file(),
        Some(workload_ca_file.as_path())
    );
    assert_eq!(
        backend.store().uri(),
        "unix:///nix/var/nix/daemon-socket/socket"
    );
    assert_eq!(backend.transfer_limits().maximum_manifest_paths(), 65_536);
    assert_eq!(backend.transfer_limits().stream_buffer_bytes(), 262_144);
    assert_eq!(backend.transfer_limits().nonce_retention().as_secs(), 600);
    let prestart = backend.prestart().expect("prestart configures");
    assert_eq!(prestart.driver(), "raw_exec");
    assert_eq!(prestart.timeout().as_secs(), 120);
    assert_eq!(prestart.resources().memory_mb(), 128);
    assert_eq!(
        prestart.driver_config()["command"],
        "/opt/operator/bin/configure-nix"
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn nomad_transfer_rejects_unprotected_hmac_secret_and_unknown_limits() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("invalid-nomad-transfer");
    let secret_file = root.join("transfer-key");
    fs::write(&secret_file, "secret\n").expect("secret writes");
    fs::set_permissions(&secret_file, fs::Permissions::from_mode(0o644))
        .expect("secret permissions set");
    let config_path = root.join("telchar.toml");
    let write_config = |extra_limit: &str| {
        fs::write(
            &config_path,
            format!(
                r#"
[[backends.nomad]]
name = "nomad"
system = "x86_64-linux"
maximum_concurrent_builds = 1
endpoint = "http://nomad.example:4646"
namespace = "telchar"
driver = "raw_exec"
job_name_scope = "prod"
poll_interval_seconds = 2
runtime_limit_seconds = 60
transfer_endpoint = "ws://telchar.example:7443"

[backends.nomad.resources]
cpu_mhz = 1000
memory_mb = 1024
disk_mb = 4096

[backends.nomad.driver_config]
command = "/opt/telchar/bin/telchar-nomad-worker"

[backends.nomad.transfer_authentication]
mode = "hmac"
key_id = "primary"
secret_file = "{}"

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
authentication_lifetime_seconds = 300
clock_skew_seconds = 30
nonce_retention_seconds = 600
reconnect_timeout_seconds = 30
maximum_diagnostic_bytes = 65536
{}
"#,
                secret_file.display(),
                extra_limit
            ),
        )
        .expect("configuration writes");
    };
    write_config("");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    assert_eq!(
        ServiceConfig::load()
            .expect_err("unsafe HMAC secret rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    fs::set_permissions(&secret_file, fs::Permissions::from_mode(0o600))
        .expect("secret permissions corrected");
    write_config("unknown_limit = 1");
    assert_eq!(
        ServiceConfig::load()
            .expect_err("unknown transfer limit rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn nomad_backend_rejects_unsafe_credentials_and_unbounded_driver_config() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("invalid-nomad");
    let token_file = root.join("nomad-token");
    fs::write(&token_file, "secret-token\n").expect("token writes");
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o644))
        .expect("token permissions set");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        format!(
            r#"
[[backends.nomad]]
name = "nomad"
system = "x86_64-linux"
maximum_concurrent_builds = 1
endpoint = "https://nomad.example:4646"
namespace = "telchar"
token_file = "{}"
driver = "docker"
job_name_scope = "prod"
poll_interval_seconds = 2
runtime_limit_seconds = 60

transfer_endpoint = "ws://telchar.example:7443"

[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "http://nomad.example:4646"
jwks_url = "http://nomad.example:4646/.well-known/jwks.json"
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
authentication_lifetime_seconds = 300
clock_skew_seconds = 30
nonce_retention_seconds = 600
reconnect_timeout_seconds = 30
maximum_diagnostic_bytes = 65536

[backends.nomad.resources]
cpu_mhz = 1000
memory_mb = 1024
disk_mb = 4096

[backends.nomad.driver_config]
image = "builder"
"#,
            token_file.display()
        ),
    )
    .expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    assert_eq!(
        ServiceConfig::load()
            .expect_err("unsafe token rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600))
        .expect("token permissions corrected");
    let oversized = "x".repeat(16_385);
    fs::write(
        &config_path,
        format!(
            r#"
[[backends.nomad]]
name = "nomad"
system = "x86_64-linux"
maximum_concurrent_builds = 1
endpoint = "https://nomad.example:4646"
namespace = "telchar"
driver = "docker"
job_name_scope = "prod"
poll_interval_seconds = 2
runtime_limit_seconds = 60

transfer_endpoint = "ws://telchar.example:7443"

[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "http://nomad.example:4646"
jwks_url = "http://nomad.example:4646/.well-known/jwks.json"
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
authentication_lifetime_seconds = 300
clock_skew_seconds = 30
nonce_retention_seconds = 600
reconnect_timeout_seconds = 30
maximum_diagnostic_bytes = 65536

[backends.nomad.resources]
cpu_mhz = 1000
memory_mb = 1024
disk_mb = 4096

[backends.nomad.driver_config]
image = "{}"
"#,
            oversized
        ),
    )
    .expect("configuration rewrites");
    assert_eq!(
        ServiceConfig::load()
            .expect_err("oversized driver configuration rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn backend_names_are_unique_across_backend_kinds() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("duplicate-backend-name");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        r#"
[backends.local]
name = "builder"
system = "x86_64-linux"
maximum_concurrent_builds = 1

[[backends.nomad]]
name = "builder"
system = "aarch64-linux"
maximum_concurrent_builds = 1
endpoint = "http://nomad.example:4646"
namespace = "telchar"
driver = "raw_exec"
job_name_scope = "prod"
poll_interval_seconds = 2
runtime_limit_seconds = 60

transfer_endpoint = "ws://telchar.example:7443"

[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "http://nomad.example:4646"
jwks_url = "http://nomad.example:4646/.well-known/jwks.json"
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
authentication_lifetime_seconds = 300
clock_skew_seconds = 30
nonce_retention_seconds = 600
reconnect_timeout_seconds = 30
maximum_diagnostic_bytes = 65536

[backends.nomad.resources]
cpu_mhz = 1000
memory_mb = 1024
disk_mb = 4096

[backends.nomad.driver_config]
command = "/opt/telchar/bin/nomad-worker"
"#,
    )
    .expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };

    assert_eq!(
        ServiceConfig::load()
            .expect_err("duplicate backend name rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn static_ssh_backend_rejects_unpinned_or_unsafe_credentials() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("invalid-static-ssh");
    let identity_file = root.join("builder-key");
    fs::write(&identity_file, "private-key").expect("identity writes");
    fs::set_permissions(&identity_file, fs::Permissions::from_mode(0o644))
        .expect("identity permissions set");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        format!(
            r#"
[[backends.static_ssh]]
name = "builder"
system = "x86_64-linux"
destination = "builder.example"
identity_file = "{}"
known_hosts_file = "relative-known-hosts"
"#,
            identity_file.display()
        ),
    )
    .expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };

    assert_eq!(
        ServiceConfig::load()
            .expect_err("unsafe SSH backend rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn static_ssh_backend_rejects_duplicate_names_and_missing_host_keys() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("ambiguous-static-ssh");
    let identity_file = root.join("builder-key");
    let known_hosts_file = root.join("known-hosts");
    fs::write(&identity_file, "private-key").expect("identity writes");
    fs::set_permissions(&identity_file, fs::Permissions::from_mode(0o600))
        .expect("identity permissions set");
    fs::write(&known_hosts_file, "\n# no pinned keys\n").expect("known hosts writes");
    let config_path = root.join("telchar.toml");
    let backend = format!(
        r#"
[[backends.static_ssh]]
name = "builder"
system = "x86_64-linux"
destination = "builder.example"
identity_file = "{}"
known_hosts_file = "{}"
"#,
        identity_file.display(),
        known_hosts_file.display()
    );
    fs::write(&config_path, format!("{backend}{backend}")).expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };

    assert_eq!(
        ServiceConfig::load()
            .expect_err("ambiguous SSH backend rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    fs::write(&config_path, &backend).expect("configuration rewrites");
    assert_eq!(
        ServiceConfig::load()
            .expect_err("known-hosts file without a key rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    fs::write(&known_hosts_file, "builder.example ssh-ed25519 AAAA\n")
        .expect("known hosts rewrites");
    fs::set_permissions(&known_hosts_file, fs::Permissions::from_mode(0o666))
        .expect("known hosts permissions set");
    assert_eq!(
        ServiceConfig::load()
            .expect_err("writable known-hosts file rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn environment_overrides_toml_scalars() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("overrides");
    let database_url_file = root.join("database-url");
    fs::write(&database_url_file, "postgresql://file/db").expect("database URL writes");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        format!(
            r#"
running_disconnect_policy = "detach-and-finish"
output_retention_seconds = 3600
[database]
url_file = "{}"
[ipc]
socket = "/run/from-file.sock"
maximum_sessions = 8
[backends.local]
name = "local"
system = "aarch64-linux"
supported_features = ["kvm"]
maximum_concurrent_builds = 1
"#,
            database_url_file.display()
        ),
    )
    .expect("configuration writes");
    unsafe {
        std::env::set_var("TELCHAR_CONFIG", &config_path);
        std::env::set_var("TELCHAR_DATABASE_URL", "postgresql://environment/db");
        std::env::set_var("TELCHAR_RUNNING_DISCONNECT_POLICY", "cancel-running");
        std::env::set_var("TELCHAR_OUTPUT_RETENTION_SECONDS", "60");
        std::env::set_var("TELCHAR_MAX_RETAINED_INPUT_BYTES", "2048");
        std::env::set_var("TELCHAR_IPC_SOCKET", "/run/from-environment.sock");
        std::env::set_var("TELCHAR_IPC_MAX_SESSIONS", "64");
    }

    let config = ServiceConfig::load().expect("configuration loads");

    assert_eq!(
        config
            .local_backend()
            .expect("local backend")
            .target()
            .system(),
        "aarch64-linux"
    );
    assert_eq!(
        config.running_disconnect_policy(),
        telchar::deployment::RunningDisconnectPolicy::CancelRunning
    );
    assert_eq!(config.output_retention().seconds(), 60);
    assert_eq!(config.database_url(), Some("postgresql://environment/db"));
    assert_eq!(
        config.ipc_socket(),
        Some(Path::new("/run/from-environment.sock"))
    );
    assert_eq!(config.maximum_ipc_sessions(), 64);
    assert_eq!(config.maximum_retained_input_bytes(), 2_048);

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn explicit_config_is_required_and_unknown_fields_fail_closed() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("invalid");
    unsafe { std::env::set_var("TELCHAR_CONFIG", root.join("missing.toml")) };
    assert_eq!(
        ServiceConfig::load()
            .expect_err("explicit missing configuration rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    let config_path = root.join("unknown.toml");
    fs::write(&config_path, "unknown = true\n").expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    assert_eq!(
        ServiceConfig::load()
            .expect_err("unknown field rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn absent_default_file_uses_policy_defaults_without_routing_targets() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();

    let config = ServiceConfig::load_from_default(Path::new("/definitely/absent/telchar.toml"))
        .expect("absent default uses policy defaults");
    assert_eq!(config.backend_targets().count(), 0);
    assert_eq!(config.output_retention().seconds(), 3600);

    restore_environment(saved);
}

#[test]
fn identity_mapping_file_replaces_toml_mapping_set() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("identity-replacement");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        r#"
[identity.credentials."ssh-pubkey:SHA256:file"]
audit_subject = "file-owner"
"#,
    )
    .expect("configuration writes");
    let mappings_path = root.join("identity.toml");
    fs::write(
        &mappings_path,
        r#"
[credentials."ssh-pubkey:SHA256:replacement"]
audit_subject = "replacement-owner"
quota_subject = "replacement-team"
"#,
    )
    .expect("mapping file writes");
    unsafe {
        std::env::set_var("TELCHAR_CONFIG", &config_path);
        std::env::set_var("TELCHAR_IDENTITY_MAPPINGS_FILE", &mappings_path);
    }

    let config = ServiceConfig::load().expect("configuration loads");

    assert!(config
        .credential_mapping("ssh-pubkey:SHA256:file")
        .is_none());
    assert_eq!(
        config
            .credential_mapping("ssh-pubkey:SHA256:replacement")
            .expect("replacement mapping exists")
            .audit_subject
            .as_deref(),
        Some("replacement-owner")
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn invalid_scheduling_limits_fail_closed() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("invalid-scheduling");
    let config_path = root.join("telchar.toml");

    for scheduling in [
        "[scheduling.default]\nmaximum_queued_builds = 0\nmaximum_active_builds = 1\n",
        "[scheduling.default]\nmaximum_queued_builds = 1\nmaximum_active_builds = 0\n",
        "[scheduling.subjects.alice]\nmaximum_queued_builds = 65537\nmaximum_active_builds = 1\n",
        "[scheduling.subjects.alice]\nmaximum_queued_builds = 1\nmaximum_active_builds = 65537\n",
    ] {
        fs::write(&config_path, scheduling).expect("configuration writes");
        unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
        assert_eq!(
            ServiceConfig::load()
                .expect_err("invalid scheduling limit rejects")
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn invalid_environment_and_mapping_values_fail_closed() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("invalid-values");
    let mapping_path = root.join("identity.toml");
    fs::write(
        &mapping_path,
        "[credentials.\"unsupported\"]\naudit_subject = \"owner\"\n",
    )
    .expect("mapping file writes");
    unsafe { std::env::set_var("TELCHAR_IDENTITY_MAPPINGS_FILE", &mapping_path) };
    assert_eq!(
        ServiceConfig::load_from_default(Path::new("/definitely/absent/telchar.toml"))
            .expect_err("unsupported credential rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    unsafe {
        std::env::remove_var("TELCHAR_IDENTITY_MAPPINGS_FILE");
        std::env::set_var("TELCHAR_IPC_MAX_SESSIONS", "0");
    }
    assert_eq!(
        ServiceConfig::load_from_default(Path::new("/definitely/absent/telchar.toml"))
            .expect_err("zero session limit rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    unsafe {
        std::env::remove_var("TELCHAR_IPC_MAX_SESSIONS");
        std::env::set_var(
            "TELCHAR_RUNNING_DISCONNECT_POLICY",
            OsString::from_vec(vec![b'x', 0x80]),
        );
    }
    assert_eq!(
        ServiceConfig::load_from_default(Path::new("/definitely/absent/telchar.toml"))
            .expect_err("non-Unicode override rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

fn fixture_root(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("telchar-service-config-{name}-{nonce}"));
    fs::create_dir(&root).expect("fixture creates");
    root
}

fn clear_environment() -> Vec<(&'static str, Option<OsString>)> {
    VARIABLES
        .iter()
        .map(|name| {
            let saved = std::env::var_os(name);
            unsafe { std::env::remove_var(name) };
            (*name, saved)
        })
        .collect()
}

fn restore_environment(saved: Vec<(&'static str, Option<OsString>)>) {
    for (name, value) in saved {
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}
