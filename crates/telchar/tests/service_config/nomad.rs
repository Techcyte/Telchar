//! Tests nomad configuration.

use super::*;

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
shutdown_drain_timeout_seconds = 11
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
    assert_eq!(callback.shutdown_drain_timeout().as_secs(), 11);
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
maximum_connection_lifetime_seconds = 3600
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
        "shutdown_drain_timeout_seconds = 0",
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

[[backends.nomad.constraints]]
attribute = "${{attr.cpu.arch}}"
operator = "="
value = "amd64"

[[backends.nomad.constraints]]
attribute = "${{node.class}}"
operator = "="
value = "general"

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
maximum_connection_lifetime_seconds = 3600
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
maximum_connection_lifetime_seconds = 3600
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
    assert_eq!(backends[0].constraints().len(), 2);
    assert_eq!(backends[0].constraints()[0].attribute(), "${attr.cpu.arch}");
    assert_eq!(backends[0].constraints()[0].operator(), "=");
    assert_eq!(backends[0].constraints()[0].value(), "amd64");
    assert_eq!(backends[0].constraints()[1].attribute(), "${node.class}");
    assert_eq!(backends[0].constraints()[1].value(), "general");
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
maximum_connection_lifetime_seconds = 3600
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
maximum_connection_lifetime_seconds = 3600
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
maximum_connection_lifetime_seconds = 3600
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
maximum_connection_lifetime_seconds = 3600
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
