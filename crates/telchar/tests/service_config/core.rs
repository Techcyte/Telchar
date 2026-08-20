//! Tests core configuration.

use std::time::Duration;

use super::*;

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
ownership_renewal_seconds = 7
ownership_lease_seconds = 28

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
        telchar::service::deployment::RunningDisconnectPolicy::CancelRunning
    );
    assert_eq!(config.output_retention().seconds(), 7200);
    assert_eq!(config.maximum_retained_input_bytes(), 1_048_576);
    assert_eq!(
        config.database_url().expect("database configured"),
        "postgresql://telchar@localhost/telchar"
    );
    assert_eq!(config.ownership_renewal_interval(), Duration::from_secs(7));
    assert_eq!(config.ownership_lease_duration(), Duration::from_secs(28));
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
        telchar::service::config::SchedulingLimits::new(20, 4).expect("limits are valid")
    );
    assert_eq!(
        config.scheduling_limits("release-engineering"),
        telchar::service::config::SchedulingLimits::new(100, 16).expect("limits are valid")
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
    assert_eq!(config.maximum_ipc_sessions(), 256);
    assert_eq!(
        config.scheduling_limits("unknown-subject"),
        telchar::service::config::SchedulingLimits::new(65_536, 65_536).expect("limits are valid")
    );

    restore_environment(saved);
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
fn invalid_database_ownership_durations_fail_closed() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("invalid-ownership-duration");
    let config_path = root.join("telchar.toml");

    for database in [
        "ownership_renewal_seconds = 0\nownership_lease_seconds = 20",
        "ownership_renewal_seconds = 5\nownership_lease_seconds = 14",
    ] {
        fs::write(&config_path, format!("[database]\n{database}\n")).expect("configuration writes");
        unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
        assert_eq!(
            ServiceConfig::load()
                .expect_err("invalid ownership duration rejects")
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
