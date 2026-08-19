//! Tests transactional additive static SSH backend reload generations.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;
use std::time::Duration;

use telchar::backend::routing::{ConfiguredBackends, ReloadableBackends};
use telchar::backend::static_ssh::{StaticSshHealth, StaticSshHealthState};
use telchar::service::config::ServiceConfig;
use telchar::service::config_reload::BackendReload;
use telchar::service::daemon_services::StaticSshHealthService;

static ENVIRONMENT: Mutex<()> = Mutex::new(());

#[test]
fn additive_reload_publishes_a_new_generation_without_mutating_existing_snapshots() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let root = tempfile::tempdir().expect("fixture creates");
    let identity = root.path().join("identity");
    let known_hosts = root.path().join("known-hosts");
    let ssh = root.path().join("ssh");
    fs::write(&identity, "identity").expect("identity writes");
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600))
        .expect("identity permissions set");
    fs::write(&known_hosts, "builder ssh-ed25519 AAAA\n").expect("known hosts writes");
    fs::write(&ssh, "#!/bin/sh\nexit 1\n").expect("SSH program writes");
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
    fs::write(&config_path, backend("builder-a")).expect("initial configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let mut current = ServiceConfig::load().expect("initial configuration loads");
    let health = StaticSshHealth::from_states(
        current.static_ssh_backends(),
        [("builder-a", StaticSshHealthState::Unavailable)],
    );
    let initial = ConfiguredBackends::with_health(&current, None, None, health.clone())
        .expect("initial backends configure");
    let old_snapshot = initial.clone();
    let reloadable = ReloadableBackends::new(initial);
    let mut health_service = StaticSshHealthService::start(health, Duration::from_secs(60))
        .expect("health service starts");

    fs::write(
        &config_path,
        format!("{}{}", backend("builder-a"), backend("builder-b")),
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
        1
    );

    assert_eq!(old_snapshot.static_ssh_health().state("builder-b"), None);
    assert_eq!(
        reloadable.snapshot().static_ssh_health().state("builder-b"),
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
