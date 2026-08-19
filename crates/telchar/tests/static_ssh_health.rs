//! Tests static SSH readiness state, immediate probing, and state-dependent scheduling.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use telchar::backend::static_ssh::{StaticSshHealth, StaticSshHealthState};
use telchar::service::config::ServiceConfig;

static ENVIRONMENT: Mutex<()> = Mutex::new(());

#[test]
fn immediate_probe_classifies_ready_and_unavailable_hosts() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let ready = fixture("ready", "#!/bin/sh\nexec nix-daemon --stdio\n");
    let unavailable = fixture("unavailable", "#!/bin/sh\nexit 255\n");
    let unreachable = fixture("unreachable", "#!/bin/sh\nsleep 30\n");
    let health =
        StaticSshHealth::probe_all(&[ready.config, unavailable.config, unreachable.config]);

    assert_eq!(health.state("ready"), Some(StaticSshHealthState::Ready));
    assert_eq!(
        health.state("unavailable"),
        Some(StaticSshHealthState::Unavailable)
    );
    assert_eq!(
        health.state("unreachable"),
        Some(StaticSshHealthState::Unavailable)
    );
    assert!(health.is_ready("ready"));
    assert!(!health.is_ready("unavailable"));
    assert!(!health.is_ready("unreachable"));
}

#[test]
fn periodic_checks_use_state_specific_intervals_and_restore_readiness() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let fixture = fixture("builder", "#!/bin/sh\nexit 255\n");
    let config = fixture.config.clone();
    let health = StaticSshHealth::from_states(
        std::slice::from_ref(&config),
        [("builder", StaticSshHealthState::Unavailable)],
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let mut probe = move |_: &_| {
        let call = observed.fetch_add(1, Ordering::SeqCst);
        if call < 2 {
            StaticSshHealthState::Unavailable
        } else {
            StaticSshHealthState::Ready
        }
    };
    let started = Instant::now();

    assert_eq!(health.check_due_with(&mut probe, started), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        health.check_due_with(&mut probe, started + Duration::from_secs(2)),
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        health.check_due_with(&mut probe, started + Duration::from_secs(4)),
        1
    );
    assert!(health.is_ready("builder"));
    assert_eq!(
        health.check_due_with(&mut probe, started + Duration::from_secs(6)),
        0,
        "ready hosts wait for their longer interval"
    );
}

struct Fixture {
    _root: std::path::PathBuf,
    config: telchar::service::config::StaticSshBackendConfig,
}

fn fixture(name: &str, script: &str) -> Fixture {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("telchar-static-ssh-health-{name}-{nonce}"));
    fs::create_dir_all(&root).expect("fixture creates");
    let identity = root.join("identity");
    let known_hosts = root.join("known-hosts");
    let ssh = root.join("ssh");
    let config_path = root.join("telchar.toml");
    fs::write(&identity, "private-key").expect("identity writes");
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600))
        .expect("identity permissions set");
    fs::write(&known_hosts, "builder ssh-ed25519 AAAA\n").expect("known hosts writes");
    fs::write(&ssh, script).expect("SSH program writes");
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755))
        .expect("SSH program permissions set");
    fs::write(
        &config_path,
        format!(
            "[[backends.static_ssh]]\nname = \"{name}\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\nready_check_interval_seconds = 300\nunavailable_check_interval_seconds = 1\ncheck_timeout_seconds = 1\ndestination = \"builder\"\nidentity_file = \"{}\"\nknown_hosts_file = \"{}\"\nssh_program = \"{}\"\n",
            identity.display(),
            known_hosts.display(),
            ssh.display()
        ),
    )
    .expect("configuration writes");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load()
        .expect("configuration loads")
        .static_ssh_backends()[0]
        .clone();
    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    Fixture {
        _root: root,
        config,
    }
}
