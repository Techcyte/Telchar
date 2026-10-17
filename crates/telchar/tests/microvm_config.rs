use telchar::service::config::MicrovmBackendConfig;

const CONFIG: &str = r#"
name = "arm"
system = "aarch64-linux"
account_id = "116392764028"
region = "us-west-2"
image_arn = "arn:aws:lambda:us-west-2:116392764028:microvm-image:builder"
image_version = "1"
maximum_concurrent_builds = 2
maximum_duration_seconds = 600
execution_timeout_seconds = 540
admission_timeout_seconds = 30
provider_timeout_seconds = 10
cleanup_interval_seconds = 5
callback_url = "wss://gateway.example/microvm"
callback_address = "192.0.2.1:443"
staging_directory = "/var/lib/telchar/microvm"
maximum_paths = 65536
maximum_nar_bytes = 1073741824
maximum_total_bytes = 2147483648
metadata_bytes = 1048576
chunk_bytes = 262144
"#;

fn service_configuration(text: &str) -> std::io::Result<telchar::service::config::ServiceConfig> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("service.toml");
    std::fs::write(&path, text).unwrap();
    telchar::service::config::ServiceConfig::load_from_default(&path)
}

#[test]
fn service_configuration_accepts_bounded_microvm_fleet() {
    let text =
        format!("[backends]\nmicrovm_bind = \"127.0.0.1:7444\"\n[[backends.microvm]]\n{CONFIG}");
    let config = service_configuration(&text).expect("MicroVM fleet configuration");
    let targets = config.backend_targets().collect::<Vec<_>>();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].name(), "arm");
    assert_eq!(targets[0].kind(), telchar::backend::BackendKind::Microvm);
    assert_eq!(targets[0].system(), "aarch64-linux");
    assert!(config.system_features().contains_key("aarch64-linux"));
    assert!(config.validate_static_ssh_reload(&config).is_ok());
    let changed =
        service_configuration(&text.replace("image_version = \"1\"", "image_version = \"2\""))
            .unwrap();
    assert!(config.validate_static_ssh_reload(&changed).is_err());
    for invalid in [
        text.replace("microvm_bind = \"127.0.0.1:7444\"", ""),
        text.replace("127.0.0.1:7444", "127.0.0.1:0"),
        text.replace("aarch64-linux", "x86_64-linux"),
        format!("{text}\n[[backends.microvm]]\n{CONFIG}"),
        format!(
            "{text}\n[backends.local]\nname = \"arm\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1"
        ),
        "[backends]\nmicrovm_bind = \"127.0.0.1:7444\"".to_owned(),
    ] {
        assert!(service_configuration(&invalid).is_err());
    }
}

#[test]
fn operator_process_reports_microvm_without_provider_or_database_access() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("service.toml");
    std::fs::write(
        &path,
        format!("[backends]\nmicrovm_bind = \"127.0.0.1:7444\"\n[[backends.microvm]]\n{CONFIG}"),
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_telchar"))
        .args(["operator", "config-check"])
        .env_clear()
        .env("TELCHAR_CONFIG", &path)
        .env("HOME", directory.path())
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["backend_count"], 1);
    assert_eq!(
        report["backends"],
        serde_json::json!([{
            "name": "arm", "kind": "microvm", "system": "aarch64-linux",
            "features": [], "capacity": 2
        }])
    );
}

#[test]
fn daemon_refuses_admission_without_provider_credentials() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let database = telchar_test_support::postgres::PostgresFixture::start();
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("service.toml");
    let socket = directory.path().join("daemon.sock");
    let callback = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = callback.local_addr().unwrap();
    std::fs::write(
        &config,
        format!("[backends]\nmicrovm_bind = \"{address}\"\n[[backends.microvm]]\n{CONFIG}"),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .args([
            "daemon",
            "--socket",
            socket.to_str().unwrap(),
            "--frontend-uid",
            &rustix::process::getuid().as_raw().to_string(),
        ])
        .env_clear()
        .env("HOME", directory.path())
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("AWS_CONFIG_FILE", directory.path().join("absent-config"))
        .env(
            "AWS_SHARED_CREDENTIALS_FILE",
            directory.path().join("absent-credentials"),
        )
        .env("TELCHAR_CONFIG", config)
        .env("TELCHAR_DATABASE_URL", database.url())
        .env(
            "TELCHAR_GATEWAY_STORE_URI",
            "unix:///unused-microvm-test-store",
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("credential rejection exceeded startup deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    assert!(!status.success());
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("MicroVM provider operation failed"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("Address already in use"),
        "attestation must precede callback binding: {stderr}"
    );
    assert!(
        !socket.exists(),
        "IPC must not open before provider attestation"
    );
    drop(callback);
    let _callback = std::net::TcpListener::bind(address).unwrap();
    let count: i64 = database
        .connect()
        .query_one("SELECT count(*) FROM singleton_ownership", &[])
        .unwrap()
        .get(0);
    assert_eq!(count, 0, "failed startup releases ownership");
}

#[test]
fn daemon_starts_attested_listener_and_releases_it_on_shutdown() {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let database = telchar_test_support::postgres::PostgresFixture::start();
    let directory = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("{error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 32768);
        }
        let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
        assert!(headers.contains("credential=fixture-key/"));
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        assert!(length < 4096);
        let mut body = vec![0; length];
        socket.read_exact(&mut body).unwrap();
        assert!(
            String::from_utf8(body)
                .unwrap()
                .contains("Action=GetCallerIdentity")
        );
        let body = "<GetCallerIdentityResponse><GetCallerIdentityResult><Account>116392764028</Account><Arn>arn:aws:iam::116392764028:user/fixture</Arn><UserId>fixture</UserId></GetCallerIdentityResult></GetCallerIdentityResponse>";
        write!(socket, "HTTP/1.1 200 OK\r\ncontent-type: text/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let callback = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = callback.local_addr().unwrap();
    drop(callback);
    let config = directory.path().join("service.toml");
    let socket = directory.path().join("daemon.sock");
    std::fs::write(
        &config,
        format!("[backends]\nmicrovm_bind = \"{address}\"\n[[backends.microvm]]\n{CONFIG}"),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .args([
            "daemon",
            "--socket",
            socket.to_str().unwrap(),
            "--frontend-uid",
            &rustix::process::getuid().as_raw().to_string(),
        ])
        .env_clear()
        .env("HOME", directory.path())
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("AWS_ACCESS_KEY_ID", "fixture-key")
        .env("AWS_SECRET_ACCESS_KEY", "fixture-secret")
        .env("TELCHAR_TEST_MICROVM_ENDPOINT", endpoint)
        .env("TELCHAR_CONFIG", config)
        .env("TELCHAR_DATABASE_URL", database.url())
        .env(
            "TELCHAR_GATEWAY_STORE_URI",
            "unix:///unused-microvm-test-store",
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            let mut error = String::new();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut error)
                .unwrap();
            panic!("daemon exited {status}: {error}");
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("startup deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    server.join().unwrap();
    assert!(std::net::TcpStream::connect(address).is_ok());
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(child.id() as i32).unwrap(),
        rustix::process::Signal::TERM,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("shutdown deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("ERROR"));
    assert!(!socket.exists());
    let _listener = std::net::TcpListener::bind(address).unwrap();
    let count: i64 = database
        .connect()
        .query_one("SELECT count(*) FROM singleton_ownership", &[])
        .unwrap()
        .get(0);
    assert_eq!(count, 0);
}

#[test]
fn deployment_configuration_preserves_execution_authority() {
    let config = MicrovmBackendConfig::parse(CONFIG).unwrap();
    assert_eq!(config.name(), "arm");
    assert_eq!(config.maximum_concurrent_builds(), 2);
    assert_eq!(config.execution_timeout().as_secs(), 540);
    assert_eq!(config.transfer_limits().chunk_bytes, 262144);
    assert_eq!(
        config.fingerprint(),
        MicrovmBackendConfig::parse(&format!("# comment\n{CONFIG}"))
            .unwrap()
            .fingerprint()
    );
    assert_ne!(
        config.fingerprint(),
        MicrovmBackendConfig::parse(
            &CONFIG.replace("image_version = \"1\"", "image_version = \"2\"")
        )
        .unwrap()
        .fingerprint()
    );
}

#[test]
fn rejects_untrusted_or_unbounded_deployment_configuration() {
    for (from, to) in [
        ("aarch64-linux", "x86_64-linux"),
        (
            "account_id = \"116392764028\"",
            "account_id = \"999999999999\"",
        ),
        ("region = \"us-west-2\"", "region = \"us-east-1\""),
        ("image_version = \"1\"", "image_version = \"latest\""),
        (
            "maximum_concurrent_builds = 2",
            "maximum_concurrent_builds = 0",
        ),
        (
            "execution_timeout_seconds = 540",
            "execution_timeout_seconds = 601",
        ),
        (
            "admission_timeout_seconds = 30",
            "admission_timeout_seconds = 540",
        ),
        (
            "provider_timeout_seconds = 10",
            "provider_timeout_seconds = 61",
        ),
        (
            "cleanup_interval_seconds = 5",
            "cleanup_interval_seconds = 0",
        ),
        (
            "wss://gateway.example/microvm",
            "ws://gateway.example/microvm",
        ),
        (
            "wss://gateway.example/microvm",
            "wss://user:secret@gateway.example/microvm",
        ),
        ("192.0.2.1:443", "0.0.0.0:443"),
        ("/var/lib/telchar/microvm", "relative/staging"),
        ("maximum_paths = 65536", "maximum_paths = 0"),
        (
            "maximum_total_bytes = 2147483648",
            "maximum_total_bytes = 1",
        ),
        ("chunk_bytes = 262144", "chunk_bytes = 262145"),
    ] {
        assert!(
            MicrovmBackendConfig::parse(&CONFIG.replace(from, to)).is_err(),
            "accepted {to}"
        );
    }
    assert!(
        MicrovmBackendConfig::parse(&format!("{CONFIG}\nendpoint_url = \"http://localhost\"\n"))
            .is_err()
    );
}
