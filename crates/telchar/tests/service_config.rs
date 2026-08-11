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
    "TELCHAR_SYSTEM",
    "TELCHAR_SUPPORTED_FEATURES",
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
[deployment]
system = "x86_64-linux"
supported_features = ["kvm", "big-parallel"]
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

    assert_eq!(config.deployment().system(), "x86_64-linux");
    assert_eq!(
        config.deployment().supported_features(),
        ["big-parallel", "kvm"]
    );
    assert_eq!(
        config.running_disconnect_policy(),
        telchar::deployment::RunningDisconnectPolicy::CancelRunning
    );
    assert_eq!(config.deployment().output_retention().seconds(), 7200);
    assert_eq!(
        config.deployment().maximum_retained_input_bytes(),
        1_048_576
    );
    assert_eq!(
        config.database_url().expect("database configured"),
        "postgresql://telchar@localhost/telchar"
    );
    assert_eq!(
        config.ipc_socket().expect("IPC socket configured"),
        Path::new("/run/telchar/daemon.sock")
    );
    assert_eq!(config.maximum_ipc_sessions(), 32);
    assert_eq!(config.backend_permit_wait().as_secs(), 30);
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
[deployment]
system = "aarch64-linux"
supported_features = ["kvm"]
running_disconnect_policy = "detach-and-finish"
output_retention_seconds = 3600
[database]
url_file = "{}"
[ipc]
socket = "/run/from-file.sock"
maximum_sessions = 8
"#,
            database_url_file.display()
        ),
    )
    .expect("configuration writes");
    unsafe {
        std::env::set_var("TELCHAR_CONFIG", &config_path);
        std::env::set_var("TELCHAR_DATABASE_URL", "postgresql://environment/db");
        std::env::set_var("TELCHAR_SYSTEM", "x86_64-linux");
        std::env::set_var("TELCHAR_SUPPORTED_FEATURES", "big-parallel,kvm");
        std::env::set_var("TELCHAR_RUNNING_DISCONNECT_POLICY", "cancel-running");
        std::env::set_var("TELCHAR_OUTPUT_RETENTION_SECONDS", "60");
        std::env::set_var("TELCHAR_MAX_RETAINED_INPUT_BYTES", "2048");
        std::env::set_var("TELCHAR_IPC_SOCKET", "/run/from-environment.sock");
        std::env::set_var("TELCHAR_IPC_MAX_SESSIONS", "64");
    }

    let config = ServiceConfig::load().expect("configuration loads");

    assert_eq!(config.deployment().system(), "x86_64-linux");
    assert_eq!(
        config.deployment().supported_features(),
        ["big-parallel", "kvm"]
    );
    assert_eq!(
        config.running_disconnect_policy(),
        telchar::deployment::RunningDisconnectPolicy::CancelRunning
    );
    assert_eq!(config.deployment().output_retention().seconds(), 60);
    assert_eq!(config.database_url(), Some("postgresql://environment/db"));
    assert_eq!(
        config.ipc_socket(),
        Some(Path::new("/run/from-environment.sock"))
    );
    assert_eq!(config.maximum_ipc_sessions(), 64);
    assert_eq!(config.deployment().maximum_retained_input_bytes(), 2_048);

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
    fs::write(
        &config_path,
        "[deployment]\nsystem = \"x86_64-linux\"\nunknown = true\n",
    )
    .expect("configuration writes");
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
fn optional_default_file_can_be_absent_and_required_values_remain_explicit() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();

    let config = ServiceConfig::load_from_default(Path::new("/definitely/absent/telchar.toml"))
        .expect("absent default configuration uses defaults");

    assert!(config.database_url().is_none());
    assert!(config.ipc_socket().is_none());
    assert_eq!(config.maximum_ipc_sessions(), 64);
    assert!(config.deployment_option().is_none());

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
        std::env::set_var("TELCHAR_SYSTEM", OsString::from_vec(vec![b'x', 0x80]));
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
