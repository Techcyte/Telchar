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
