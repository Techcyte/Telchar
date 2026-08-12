use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use telchar::backend::{BuildBackend, BuildExecution};
use telchar::config::ServiceConfig;
use telchar::static_ssh_backend::{verify_configured_backends, StaticSshBackend};
use telchar::store_daemon::GatewayStoreEndpoint;

#[path = "support/build_request.rs"]
mod build_request_support;

use build_request_support::admitted_request;

#[test]
fn static_ssh_executor_implements_backend_contract_with_configured_transport() {
    fn assert_backend<T: BuildBackend>() {}
    assert_backend::<StaticSshBackend>();

    let root = std::env::temp_dir().join(format!(
        "telchar-static-ssh-backend-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("fixture root creates");
    let identity = root.join("identity");
    let known_hosts = root.join("known-hosts");
    let ssh = root.join("ssh");
    let config_path = root.join("telchar.toml");
    fs::write(&identity, "private-key").expect("identity writes");
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600))
        .expect("identity permissions set");
    fs::write(&known_hosts, "builder ssh-ed25519 AAAA\n").expect("known hosts writes");
    fs::write(&ssh, "#!/bin/sh\nexit 1\n").expect("SSH program writes");
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755))
        .expect("SSH program permissions set");
    fs::write(
        &config_path,
        format!(
            "[[backends.static_ssh]]\nname = \"builder\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\ndestination = \"telchar-builder@builder\"\nidentity_file = \"{}\"\nknown_hosts_file = \"{}\"\nssh_program = \"{}\"\n",
            identity.display(),
            known_hosts.display(),
            ssh.display()
        ),
    )
    .expect("configuration writes");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load().expect("configuration loads");
    let backend = StaticSshBackend::new(
        config.static_ssh_backends()[0].clone(),
        GatewayStoreEndpoint::parse("unix:///run/nix-daemon.sock").expect("endpoint parses"),
    );
    let _backend: &dyn BuildBackend = &backend;
    let _execution_type = std::any::TypeId::of::<BuildExecution<'static>>();
    let _timeout = Duration::from_secs(30);

    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn hostile_transport_diagnostics_do_not_expose_credentials_or_destination() {
    let root = std::env::temp_dir().join(format!(
        "telchar-static-ssh-redaction-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("fixture root creates");
    let identity = root.join("secret-identity");
    let known_hosts = root.join("known-hosts");
    let ssh = root.join("ssh");
    let config_path = root.join("telchar.toml");
    fs::write(&identity, "private-key").expect("identity writes");
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600))
        .expect("identity permissions set");
    fs::write(&known_hosts, "builder ssh-ed25519 AAAA\n").expect("known hosts writes");
    fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nprintf '%s\\n' 'authentication failed for {} using {}' >&2\nsleep 0.05\nexit 1\n",
            "telchar-builder@builder",
            identity.display()
        ),
    )
    .expect("SSH program writes");
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755))
        .expect("SSH program permissions set");
    fs::write(
        &config_path,
        format!(
            "[[backends.static_ssh]]\nname = \"builder\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\ndestination = \"telchar-builder@builder\"\nidentity_file = \"{}\"\nknown_hosts_file = \"{}\"\nssh_program = \"{}\"\n",
            identity.display(),
            known_hosts.display(),
            ssh.display()
        ),
    )
    .expect("configuration writes");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load().expect("configuration loads");
    let mut backend = StaticSshBackend::new(
        config.static_ssh_backends()[0].clone(),
        GatewayStoreEndpoint::parse("unix:///run/nix-daemon.sock").expect("endpoint parses"),
    );
    let build = admitted_request();
    let execution = BuildExecution::new("request-1", &build, Duration::from_secs(1))
        .expect("execution is valid");
    let mut logs = Vec::new();

    backend
        .execute_with_logs(
            &execution,
            &mut |chunk| {
                logs.extend_from_slice(chunk);
                Ok(())
            },
            &mut || Ok(false),
        )
        .expect_err("hostile transport fails");

    let logs = String::from_utf8(logs).expect("logs are UTF-8");
    assert_eq!(logs, "static SSH transport diagnostic\n");
    assert!(!logs.contains("telchar-builder@builder"), "{logs}");
    assert!(
        !logs.contains(identity.to_str().expect("identity path is UTF-8")),
        "{logs}"
    );

    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn startup_verification_requires_a_compatible_reachable_nix_daemon() {
    let root = std::env::temp_dir().join(format!(
        "telchar-static-ssh-verification-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("fixture root creates");
    let identity = root.join("identity");
    let known_hosts = root.join("known-hosts");
    let ssh = root.join("ssh");
    let config_path = root.join("telchar.toml");
    fs::write(&identity, "private-key").expect("identity writes");
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600))
        .expect("identity permissions set");
    fs::write(&known_hosts, "builder ssh-ed25519 AAAA\n").expect("known hosts writes");
    fs::write(&ssh, "#!/bin/sh\nexec nix-daemon --stdio\n").expect("SSH program writes");
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755))
        .expect("SSH program permissions set");
    fs::write(
        &config_path,
        format!(
            "[[backends.static_ssh]]\nname = \"builder\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\ndestination = \"telchar-builder@builder\"\nidentity_file = \"{}\"\nknown_hosts_file = \"{}\"\nssh_program = \"{}\"\n",
            identity.display(),
            known_hosts.display(),
            ssh.display()
        ),
    )
    .expect("configuration writes");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load().expect("configuration loads");

    verify_configured_backends(config.static_ssh_backends(), Duration::from_secs(5))
        .expect("reachable compatible daemon verifies");

    fs::write(&ssh, "#!/bin/sh\nexit 1\n").expect("failing SSH program writes");
    assert!(
        verify_configured_backends(config.static_ssh_backends(), Duration::from_secs(1)).is_err()
    );

    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    fs::remove_dir_all(root).expect("fixture removes");
}
