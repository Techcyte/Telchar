//! Tests static SSH readiness state, immediate probing, and state-dependent scheduling.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use telchar::backend::static_ssh::{StaticSshHealth, StaticSshHealthState};
use telchar::service::config::ServiceConfig;

#[test]
fn immediate_probe_classifies_ready_and_unavailable_hosts() {
    let ready = fixture("ready", "#!/bin/sh\nexec nix-daemon --stdio\n");
    let unavailable = fixture("unavailable", "#!/bin/sh\nexit 255\n");
    let unreachable = fixture("unreachable", "#!/bin/sh\nsleep 30\n");
    let health =
        StaticSshHealth::probe_all(&[ready.config, unavailable.config, unreachable.config]);

    assert_eq!(
        health.state("pool.ready"),
        Some(StaticSshHealthState::Ready)
    );
    assert_eq!(
        health.state("pool.unavailable"),
        Some(StaticSshHealthState::Unavailable)
    );
    assert_eq!(
        health.state("pool.unreachable"),
        Some(StaticSshHealthState::Unavailable)
    );
    assert!(health.is_ready("pool.ready"));
    assert!(!health.is_ready("pool.unavailable"));
    assert!(!health.is_ready("pool.unreachable"));
}

#[test]
fn initial_inventory_probes_run_concurrently_with_four_process_limit() {
    let observations = tempfile::tempdir().unwrap();
    let fixtures = (0..12)
        .map(|index| {
            fixture(
                &format!("builder{index}"),
                &format!(
                    "#!/bin/sh\ndate +%s%N > '{}/start-{index}'\nsleep 0.2\ndate +%s%N > '{}/end-{index}'\nexit 255\n",
                    observations.path().display(),
                    observations.path().display(),
                ),
            )
        })
        .collect::<Vec<_>>();
    let backends = fixtures
        .iter()
        .map(|fixture| fixture.config.clone())
        .collect::<Vec<_>>();
    let health = StaticSshHealth::probe_all(&backends);
    let mut events = Vec::new();
    for index in 0..12 {
        assert_eq!(
            health.state(&format!("pool.builder{index}")),
            Some(StaticSshHealthState::Unavailable)
        );
        for (name, delta) in [("start", 1), ("end", -1)] {
            let timestamp = fs::read_to_string(observations.path().join(format!("{name}-{index}")))
                .unwrap()
                .trim()
                .parse::<u128>()
                .unwrap();
            events.push((timestamp, delta));
        }
    }
    events.sort_unstable();
    let mut active = 0;
    let mut maximum = 0;
    for (_, delta) in events {
        active += delta;
        maximum = maximum.max(active);
    }
    assert!(
        (2..=4).contains(&maximum),
        "observed {maximum} concurrent probe processes"
    );
    assert_eq!(active, 0);
}

#[test]
fn periodic_checks_use_state_specific_intervals_and_restore_readiness() {
    let fixture = fixture("builder", "#!/bin/sh\nexit 255\n");
    let config = fixture.config.clone();
    let health = StaticSshHealth::from_states(
        std::slice::from_ref(&config),
        [("pool.builder", StaticSshHealthState::Unavailable)],
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
    assert!(health.is_ready("pool.builder"));
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
            "[[backends.ssh]]\nready_check_interval_seconds = 300\nunavailable_check_interval_seconds = 1\ncheck_timeout_seconds = 1\nidentity_file = \"{}\"\nknown_hosts_file = \"{}\"\nssh_program = \"{}\"\n[backends.ssh.pool]\nsource = \"static\"\n[backends.ssh.pool.{name}]\naddress = \"builder\"\n",
            identity.display(),
            known_hosts.display(),
            ssh.display()
        ),
    )
    .expect("configuration writes");
    let config = ServiceConfig::load_with_environment(&config_path, |_| None)
        .expect("configuration loads")
        .static_ssh_backends()[0]
        .clone();
    Fixture {
        _root: root,
        config,
    }
}
