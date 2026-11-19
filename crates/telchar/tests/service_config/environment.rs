//! Tests environment configuration.

use super::*;

#[test]
fn environment_overrides_toml_scalars() {
    let mut environment = Environment::default();
    let root = fixture_root("overrides");
    let database_url_file = root.join("database-url");
    fs::write(&database_url_file, "postgresql://file/db").expect("database URL writes");
    fs::set_permissions(&database_url_file, fs::Permissions::from_mode(0o600))
        .expect("database URL permissions set");
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
    environment.set("TELCHAR_CONFIG", &config_path);
    environment.set("TELCHAR_DATABASE_URL", "postgresql://environment/db");
    environment.set("TELCHAR_RUNNING_DISCONNECT_POLICY", "cancel-running");
    environment.set("TELCHAR_OUTPUT_RETENTION_SECONDS", "60");
    environment.set("TELCHAR_MAX_RETAINED_INPUT_BYTES", "2048");
    environment.set("TELCHAR_IPC_SOCKET", "/run/from-environment.sock");
    environment.set("TELCHAR_IPC_MAX_SESSIONS", "64");

    let config = load(&environment).expect("configuration loads");

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
        telchar::service::deployment::RunningDisconnectPolicy::CancelRunning
    );
    assert_eq!(config.output_retention().seconds(), 60);
    assert_eq!(config.database_url(), Some("postgresql://environment/db"));
    assert_eq!(
        config.ipc_socket(),
        Some(Path::new("/run/from-environment.sock"))
    );
    assert_eq!(config.maximum_ipc_sessions(), 64);
    assert_eq!(config.maximum_retained_input_bytes(), 2_048);

    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn identity_mapping_file_replaces_toml_mapping_set() {
    let mut environment = Environment::default();
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
    environment.set("TELCHAR_CONFIG", &config_path);
    environment.set("TELCHAR_IDENTITY_MAPPINGS_FILE", &mappings_path);

    let config = load(&environment).expect("configuration loads");

    assert!(
        config
            .credential_mapping("ssh-pubkey:SHA256:file")
            .is_none()
    );
    assert_eq!(
        config
            .credential_mapping("ssh-pubkey:SHA256:replacement")
            .expect("replacement mapping exists")
            .audit_subject
            .as_deref(),
        Some("replacement-owner")
    );

    fs::remove_dir_all(root).expect("fixture removes");
}
