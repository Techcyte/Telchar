//! Tests static ssh configuration.

use super::*;

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
ready_check_interval_seconds = 600
unavailable_check_interval_seconds = 30
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
    assert_eq!(backend.ready_check_interval().as_secs(), 600);
    assert_eq!(backend.unavailable_check_interval().as_secs(), 30);
    assert_eq!(backend.destination(), "telchar-builder@builder.example");
    assert_eq!(backend.identity_file(), identity_file);
    assert_eq!(backend.known_hosts_file(), known_hosts_file);
    assert_eq!(backend.ssh_program(), ssh_program);

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn reload_accepts_static_ssh_inventory_additions_and_removals() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("static-ssh-reload");
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
    let backend = |name: &str, capacity: usize| {
        format!(
            "[[backends.static_ssh]]\nname = \"{name}\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = {capacity}\ndestination = \"{name}\"\nidentity_file = \"{}\"\nknown_hosts_file = \"{}\"\nssh_program = \"{}\"\n",
            identity_file.display(),
            known_hosts_file.display(),
            ssh_program.display()
        )
    };
    fs::write(&config_path, backend("builder-a", 1)).expect("initial configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let original = ServiceConfig::load().expect("initial configuration loads");

    fs::write(
        &config_path,
        format!("{}{}", backend("builder-a", 1), backend("builder-b", 2)),
    )
    .expect("additive configuration writes");
    let additive = ServiceConfig::load().expect("additive configuration loads");
    assert_eq!(
        original
            .validate_static_ssh_reload(&additive)
            .expect("additive reload validates"),
        telchar::service::config::StaticSshReloadChanges {
            added: 1,
            removed: 0,
        }
    );

    fs::write(&config_path, backend("builder-a", 2)).expect("changed configuration writes");
    let changed = ServiceConfig::load().expect("changed configuration loads");
    assert!(original.validate_static_ssh_reload(&changed).is_err());

    fs::write(&config_path, "").expect("removed configuration writes");
    let removed = ServiceConfig::load().expect("removed configuration loads");
    assert_eq!(
        original
            .validate_static_ssh_reload(&removed)
            .expect("removal reload validates"),
        telchar::service::config::StaticSshReloadChanges {
            added: 0,
            removed: 1,
        }
    );

    restore_environment(saved);
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn static_ssh_backend_defaults_health_check_intervals() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = clear_environment();
    let root = fixture_root("static-ssh-health-defaults");
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
            "[[backends.static_ssh]]\nname = \"builder\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\ndestination = \"builder.example\"\nidentity_file = \"{}\"\nknown_hosts_file = \"{}\"\nssh_program = \"{}\"\n",
            identity_file.display(),
            known_hosts_file.display(),
            ssh_program.display()
        ),
    )
    .expect("configuration writes");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };

    let config = ServiceConfig::load().expect("configuration loads");
    let backend = &config.static_ssh_backends()[0];
    assert_eq!(backend.ready_check_interval().as_secs(), 300);
    assert_eq!(backend.unavailable_check_interval().as_secs(), 60);
    assert_eq!(backend.check_timeout().as_secs(), 10);

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
