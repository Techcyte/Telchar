use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use telchar::backend::{BuildBackend, BuildExecution};
use telchar::config::ServiceConfig;
use telchar::static_ssh_backend::StaticSshBackend;
use telchar::store_daemon::GatewayStoreEndpoint;

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
