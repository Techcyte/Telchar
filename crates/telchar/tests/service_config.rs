//! Tests service config contracts and failure boundaries, including loads strict toml and identity mappings.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use telchar::backend::BackendKind;
use telchar::service::config::ServiceConfig;

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

#[path = "service_config/core.rs"]
mod core;
#[path = "service_config/environment.rs"]
mod environment;
#[path = "service_config/nomad.rs"]
mod nomad;
#[path = "service_config/static_ssh.rs"]
mod static_ssh;
#[path = "service_config/static_ssh_consul.rs"]
mod static_ssh_consul;

#[test]
fn fixture_suffixes_differ_when_clock_values_match() {
    assert_ne!(fixture_suffix(1), fixture_suffix(1));
}

fn fixture_root(name: &str) -> PathBuf {
    let nonce = fixture_suffix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos(),
    );
    let root = std::env::temp_dir().join(format!("telchar-service-config-{name}-{nonce}"));
    fs::create_dir(&root).expect("fixture creates");
    root
}

fn fixture_suffix(clock_nanoseconds: u128) -> u128 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    clock_nanoseconds.wrapping_add(u128::from(SEQUENCE.fetch_add(1, Ordering::Relaxed)))
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

#[test]
fn signing_is_optional_and_requires_protected_key_and_known_failure_policy() {
    let _guard = ENVIRONMENT.lock().unwrap();
    let saved = clear_environment();
    let root = fixture_root("signing");
    let config = root.join("config.toml");
    fs::write(&config, "").unwrap();
    assert!(
        ServiceConfig::load_from_default(&config)
            .unwrap()
            .signing()
            .is_none()
    );
    let key = root.join("key");
    fs::write(&key, "never-log-this-secret").unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&config, format!("[signing]\nkey_file = {:?}\n", key)).unwrap();
    let error = ServiceConfig::load_from_default(&config).unwrap_err();
    assert!(!error.to_string().contains("never-log"));
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let loaded = ServiceConfig::load_from_default(&config).unwrap();
    assert_eq!(
        loaded.signing().unwrap().failure_policy(),
        telchar::service::output_signing::SigningFailurePolicy::Warn
    );
    fs::write(
        &config,
        format!(
            "[signing]\nkey_file = {:?}\nfailure_policy = 'fail-build'\n",
            key
        ),
    )
    .unwrap();
    assert_eq!(
        ServiceConfig::load_from_default(&config)
            .unwrap()
            .signing()
            .unwrap()
            .failure_policy(),
        telchar::service::output_signing::SigningFailurePolicy::FailBuild
    );
    fs::write(
        &config,
        format!(
            "[signing]\nkey_file = {:?}\nfailure_policy = 'ignore'\n",
            key
        ),
    )
    .unwrap();
    assert!(ServiceConfig::load_from_default(&config).is_err());
    fs::remove_dir_all(root).unwrap();
    restore_environment(saved);
}
