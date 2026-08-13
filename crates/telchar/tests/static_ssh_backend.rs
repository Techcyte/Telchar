//! Tests static ssh backend contracts and failure boundaries, including load config.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use telchar::backend::{BuildBackend, BuildExecution};
use telchar::config::ServiceConfig;
use telchar::static_ssh_backend::{StaticSshBackend, verify_configured_backends};
use telchar::store_daemon::GatewayStoreEndpoint;

#[path = "support/build_request.rs"]
mod build_request_support;

use build_request_support::admitted_request;

static CONFIG_ENVIRONMENT: Mutex<()> = Mutex::new(());

fn load_config<T>(path: &Path, select: impl FnOnce(ServiceConfig) -> T) -> T {
    let _guard = CONFIG_ENVIRONMENT
        .lock()
        .expect("configuration environment lock");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", path) };
    let config = select(ServiceConfig::load().expect("configuration loads"));
    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    config
}

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
    let config = load_config(&config_path, std::convert::identity);
    let backend = StaticSshBackend::new(
        config.static_ssh_backends()[0].clone(),
        GatewayStoreEndpoint::parse("unix:///run/nix-daemon.sock").expect("endpoint parses"),
    );
    let _backend: &dyn BuildBackend = &backend;
    let _execution_type = std::any::TypeId::of::<BuildExecution<'static>>();
    let _timeout = Duration::from_secs(30);

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
    let config = load_config(&config_path, std::convert::identity);
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

    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn timeout_terminates_the_static_ssh_process_group() {
    let fixture = BlockingTransportFixture::new("timeout");
    let mut backend = fixture.backend();
    let build = admitted_request();
    let execution = BuildExecution::new("request-timeout", &build, Duration::from_millis(50))
        .expect("execution is valid");

    let error = backend
        .execute_with_logs(&execution, &mut |_| Ok(()), &mut || Ok(false))
        .expect_err("blocked transport times out");

    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    fixture.assert_descendant_stopped();
}

#[test]
fn cancellation_terminates_the_static_ssh_process_group() {
    let fixture = BlockingTransportFixture::new("cancel");
    let mut backend = fixture.backend();
    let build = admitted_request();
    let execution = BuildExecution::new("request-cancel", &build, Duration::from_secs(1))
        .expect("execution is valid");

    let error = backend
        .execute_with_logs(&execution, &mut |_| Ok(()), &mut || {
            Ok(fixture.pid_path.exists())
        })
        .expect_err("cancelled transport stops");

    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
    fixture.assert_descendant_stopped();
}

struct BlockingTransportFixture {
    root: std::path::PathBuf,
    config: telchar::config::StaticSshBackendConfig,
    pid_path: std::path::PathBuf,
}

impl BlockingTransportFixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "telchar-static-ssh-{name}-{}-{}",
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
        let pid_path = root.join("descendant-pid");
        let config_path = root.join("telchar.toml");
        fs::write(&identity, "private-key").expect("identity writes");
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o600))
            .expect("identity permissions set");
        fs::write(&known_hosts, "builder ssh-ed25519 AAAA\n").expect("known hosts writes");
        fs::write(
            &ssh,
            format!(
                "#!/bin/sh\ntrap 'exit 0' TERM INT\nsh -c 'trap \"exit 0\" TERM INT; printf %s $$ > \"{}\"; while :; do sleep 1; done' &\nwait\n",
                pid_path.display()
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
        let config = load_config(&config_path, |config| {
            config.static_ssh_backends()[0].clone()
        });
        Self {
            root,
            config,
            pid_path,
        }
    }

    fn backend(&self) -> StaticSshBackend {
        StaticSshBackend::new(
            self.config.clone(),
            GatewayStoreEndpoint::parse("unix:///run/nix-daemon.sock").expect("endpoint parses"),
        )
    }

    fn assert_descendant_stopped(&self) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !self.pid_path.exists() {
            assert!(
                Instant::now() < deadline,
                "transport descendant did not start"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let pid = fs::read_to_string(&self.pid_path).expect("descendant PID reads");
        let status = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("process liveness query runs");
        assert!(!status.success(), "static SSH descendant remains alive");
    }
}

impl Drop for BlockingTransportFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn malformed_worker_protocol_fails_output_recovery_cleanly() {
    let fixture = RecoveryTransportFixture::new("malformed", "printf 'not-worker-protocol'");

    let error = telchar::static_ssh_backend::recover_outputs(
        &fixture.config,
        &GatewayStoreEndpoint::parse("unix:///run/nix-daemon.sock").expect("endpoint parses"),
        &["/nix/store/11111111111111111111111111111111-static-ssh-output".to_owned()],
        Duration::from_secs(1),
    )
    .expect_err("malformed worker protocol fails");

    assert_ne!(error.kind(), std::io::ErrorKind::TimedOut);
}

#[test]
fn missing_exact_remote_output_fails_before_gateway_import() {
    let fixture = RecoveryTransportFixture::new("missing-output", "exec nix-daemon --stdio");
    let missing = format!(
        "/nix/store/11111111111111111111111111111111-telchar-missing-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos()
    );

    let error = telchar::static_ssh_backend::recover_outputs(
        &fixture.config,
        &GatewayStoreEndpoint::parse("unix:///run/nix-daemon.sock").expect("endpoint parses"),
        &[missing],
        Duration::from_secs(5),
    )
    .expect_err("missing remote output fails");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "remote output is unavailable");
}

struct RecoveryTransportFixture {
    root: std::path::PathBuf,
    config: telchar::config::StaticSshBackendConfig,
}

impl RecoveryTransportFixture {
    fn new(name: &str, body: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "telchar-static-ssh-{name}-{}-{}",
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
        fs::write(&ssh, format!("#!/bin/sh\n{body}\n")).expect("SSH program writes");
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
        let config = load_config(&config_path, |config| {
            config.static_ssh_backends()[0].clone()
        });
        Self { root, config }
    }
}

impl Drop for RecoveryTransportFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn output_recovery_timeout_terminates_the_static_ssh_process_group() {
    let fixture = BlockingTransportFixture::new("recovery-timeout");

    let error = telchar::static_ssh_backend::recover_outputs(
        &fixture.config,
        &GatewayStoreEndpoint::parse("unix:///run/nix-daemon.sock").expect("endpoint parses"),
        &["/nix/store/11111111111111111111111111111111-static-ssh-output".to_owned()],
        Duration::from_millis(50),
    )
    .expect_err("blocked recovery times out");

    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    fixture.assert_descendant_stopped();
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
    let config = load_config(&config_path, std::convert::identity);

    verify_configured_backends(config.static_ssh_backends(), Duration::from_secs(5))
        .expect("reachable compatible daemon verifies");

    fs::write(&ssh, "#!/bin/sh\nexit 1\n").expect("failing SSH program writes");
    assert!(
        verify_configured_backends(config.static_ssh_backends(), Duration::from_secs(1)).is_err()
    );

    fs::remove_dir_all(root).expect("fixture removes");
}
