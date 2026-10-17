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
    daemon_startup_rejection(false);
}

#[test]
fn daemon_refuses_unconfigured_cleanup_authority() {
    daemon_startup_rejection(true);
}

fn daemon_startup_rejection(retained_cleanup: bool) {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let database = telchar_test_support::postgres::PostgresFixture::start();
    if retained_cleanup {
        telchar::persistence::migrate(database.url()).unwrap();
        let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-cleanup.drv";
        let kind = telchar::backend::BackendKind::Microvm;
        telchar::persistence::claim_shared_build(
            database.url(),
            derivation,
            &[1; 32],
            "removed-policy",
            kind,
            kind.capabilities(),
            None,
            &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-cleanup"],
        )
        .unwrap();
        telchar::persistence::start_shared_build(database.url(), derivation).unwrap();
        let attempt = telchar::persistence::read_shared_build_attempt(database.url(), derivation)
            .unwrap()
            .unwrap();
        database.connect().execute(
            "INSERT INTO microvm_launches (intent_id, attempt_id, backend_name, account_id, region, image_arn, image_version, configuration_digest, request_digest, client_token, credential_verifier, admission_cutoff, maximum_duration_seconds, phase, provider_id) VALUES ('retained', $1, 'removed-policy', '000000000000', 'us-west-2', 'fixture', '1', $2, $2, 'retained-token', $2, clock_timestamp() + interval '1 minute', 600, 'closed', 'microvm-retained')",
            &[&attempt.attempt_id, &&[1_u8; 32][..]],
        ).unwrap();
    }
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
            panic!("startup rejection exceeded deadline");
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
    let expected = if retained_cleanup {
        "MicroVM cleanup requires configured account and region"
    } else {
        "MicroVM provider operation failed"
    };
    assert!(stderr.contains(expected), "{stderr}");
    if retained_cleanup {
        assert!(
            !stderr.contains("MicroVM provider operation failed"),
            "{stderr}"
        );
        let retained: bool = database.connect().query_one("SELECT reservation_released_at IS NULL AND provider_id = 'microvm-retained' FROM microvm_launches WHERE intent_id = 'retained'", &[]).unwrap().get(0);
        assert!(retained, "startup refusal preserves cleanup obligation");
    }
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
    daemon_lifecycle(DaemonExit::Terminated);
}

#[test]
fn daemon_closes_attested_listener_after_ownership_loss() {
    daemon_lifecycle(DaemonExit::Fenced);
}

#[test]
fn daemon_rejects_signed_identity_for_another_account() {
    daemon_lifecycle(DaemonExit::AccountMismatch);
}

#[derive(Clone, Copy, PartialEq)]
enum DaemonExit {
    Terminated,
    Fenced,
    AccountMismatch,
}

fn daemon_lifecycle(exit: DaemonExit) {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let database = telchar_test_support::postgres::PostgresFixture::start();
    let directory = tempfile::tempdir().unwrap();
    let store = (exit == DaemonExit::Terminated)
        .then(|| telchar::fixture::nix::NixFixture::create().unwrap());
    let store_daemon = store.as_ref().map(|store| {
        store
            .start_daemon(telchar::fixture::nix::TrustMode::Trusted)
            .unwrap()
    });
    if let Some(daemon) = &store_daemon {
        daemon.worker_client_profile().expect("isolated store handshake");
    }
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
        let account = if exit == DaemonExit::AccountMismatch {
            "000000000000"
        } else {
            "116392764028"
        };
        let body = format!(
            "<GetCallerIdentityResponse><GetCallerIdentityResult><Account>{account}</Account><Arn>arn:aws:iam::{account}:user/fixture</Arn><UserId>fixture</UserId></GetCallerIdentityResult></GetCallerIdentityResponse>"
        );
        write!(socket, "HTTP/1.1 200 OK\r\ncontent-type: text/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        if exit == DaemonExit::Terminated {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "daemon must dispatch admitted build"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 32768);
            }
            let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            assert!(
                headers.starts_with("post /2025-09-09/microvms "),
                "{headers}"
            );
            assert!(headers.contains("credential=fixture-key/"));
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            assert!(length < 32768);
            let mut body = vec![0; length];
            socket.read_exact(&mut body).unwrap();
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(request["runHookPayload"].as_str().is_some());
            socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}").unwrap();
        }
    });
    let callback = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = callback.local_addr().unwrap();
    let callback = (exit == DaemonExit::AccountMismatch).then_some(callback);
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
            store_daemon
                .as_ref()
                .map(|daemon| format!("unix://{}", daemon.socket_path().display()))
                .unwrap_or_else(|| "unix:///unused-microvm-test-store".into()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            if exit == DaemonExit::AccountMismatch {
                break;
            }
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
    if exit != DaemonExit::AccountMismatch {
        assert!(std::net::TcpStream::connect(address).is_ok());
    }
    if exit != DaemonExit::AccountMismatch {
        use telchar::service::ipc::{IPC_VERSION, IpcEnvelope, IpcListener, RequesterMetadata};
        let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        IpcListener::send_envelope(
            &mut stream,
            &IpcEnvelope {
                version: IPC_VERSION,
                requester: RequesterMetadata {
                    credential_id: "ssh-pubkey:microvm-fixture".into(),
                    audit_subject: "microvm-fixture".into(),
                    quota_subject: "microvm-fixture".into(),
                },
                session_id: "microvm-ipc".into(),
                trace_context: telchar_telemetry::TraceContext::default(),
                error: None,
            },
        )
        .unwrap();
        let mut request_stream = stream.try_clone().unwrap();
        let client = nix_worker_protocol::WorkerClient::connect(stream).unwrap();
        assert_eq!(
            client.profile().version,
            nix_worker_protocol::LATEST_WORKER_VERSION
        );
        if exit == DaemonExit::Terminated {
            fn word(stream: &mut impl Write, value: u64) {
                stream.write_all(&value.to_le_bytes()).unwrap();
            }
            fn bytes(stream: &mut impl Write, value: &[u8]) {
                word(stream, value.len() as u64);
                stream.write_all(value).unwrap();
                stream
                    .write_all(&[0; 7][..(8 - value.len() % 8) % 8])
                    .unwrap();
            }
            fn read_word(stream: &mut impl Read) -> u64 {
                let mut value = [0; 8];
                stream.read_exact(&mut value).unwrap();
                u64::from_le_bytes(value)
            }
            fn read_bytes(stream: &mut impl Read) -> Vec<u8> {
                let length = read_word(stream) as usize;
                assert!(length < 4096);
                let mut value = vec![0; length];
                stream.read_exact(&mut value).unwrap();
                stream
                    .read_exact(&mut [0; 7][..(8 - length % 8) % 8])
                    .unwrap();
                value
            }
            word(&mut request_stream, 36);
            bytes(
                &mut request_stream,
                b"/nix/store/00000000000000000000000000000000-ipc-build.drv",
            );
            word(&mut request_stream, 1);
            for value in [
                b"out".as_slice(),
                b"/nix/store/11111111111111111111111111111111-ipc-build",
                b"",
                b"",
            ] {
                bytes(&mut request_stream, value);
            }
            word(&mut request_stream, 0);
            bytes(&mut request_stream, b"aarch64-linux");
            bytes(&mut request_stream, b"/bin/sh");
            word(&mut request_stream, 2);
            bytes(&mut request_stream, b"-c");
            bytes(&mut request_stream, b"printf fixture > $out");
            word(&mut request_stream, 4);
            for (key, value) in [
                (b"builder".as_slice(), b"/bin/sh".as_slice()),
                (b"name", b"ipc-build"),
                (
                    b"out",
                    b"/nix/store/11111111111111111111111111111111-ipc-build",
                ),
                (b"system", b"aarch64-linux"),
            ] {
                bytes(&mut request_stream, key);
                bytes(&mut request_stream, value);
            }
            word(&mut request_stream, 0);
            request_stream.flush().unwrap();
            assert_eq!(
                read_word(&mut request_stream),
                nix_worker_protocol::STDERR_ERROR
            );
            assert_eq!(read_bytes(&mut request_stream), b"Error");
            let _level = read_word(&mut request_stream);
            assert_eq!(read_bytes(&mut request_stream), b"Error");
            let rejection = read_bytes(&mut request_stream);
            if rejection != b"BuildDerivation execution failed" {
                child.kill().unwrap();
                child.wait().unwrap();
                let mut diagnostic = String::new();
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut diagnostic)
                    .unwrap();
                panic!(
                    "unexpected rejection {:?}: {diagnostic}",
                    String::from_utf8_lossy(&rejection)
                );
            }
            assert_eq!(read_word(&mut request_stream), 0);
            assert_eq!(read_word(&mut request_stream), 0);
            let launches: i64 = database
                .connect()
                .query_one("SELECT count(*) FROM microvm_launches", &[])
                .unwrap()
                .get(0);
            assert_eq!(launches, 1, "daemon persists one provider launch intent");
        }
        drop(request_stream);
        drop(client);
        let session = database.connect().query_one(
            "SELECT credential_id, audit_subject, quota_subject FROM protocol_sessions WHERE session_id = 'microvm-ipc'", &[],
        ).unwrap();
        assert_eq!(
            session.get::<_, Option<String>>(0).as_deref(),
            Some("ssh-pubkey:microvm-fixture")
        );
        assert_eq!(session.get::<_, String>(1), "microvm-fixture");
        assert_eq!(session.get::<_, String>(2), "microvm-fixture");
    }
    if exit == DaemonExit::Fenced {
        database.expire_singleton_ownership("daemon");
    } else if exit == DaemonExit::Terminated {
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(child.id() as i32).unwrap(),
            rustix::process::Signal::TERM,
        )
        .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("shutdown deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    server.join().unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.success(),
        exit == DaemonExit::Terminated,
        "{stderr}"
    );
    assert!(output.stdout.is_empty());
    if exit == DaemonExit::AccountMismatch {
        assert!(
            stderr.contains("MicroVM provider operation failed"),
            "{stderr}"
        );
        assert!(!stderr.contains("Address already in use"), "{stderr}");
    } else if exit == DaemonExit::Fenced {
        assert!(
            stderr.contains("telchar: singleton ownership operation failed"),
            "{stderr}"
        );
    } else {
        let errors: Vec<_> = stderr
            .lines()
            .filter(|line| line.contains(" ERROR "))
            .collect();
        assert_eq!(errors.len(), 2, "{stderr}");
        assert!(
            errors[0].contains("event=\"gateway.store_retention.failed\""),
            "{stderr}"
        );
        assert!(
            errors[0].contains("gateway Nix daemon connection failed"),
            "{stderr}"
        );
        assert!(
            errors[1].contains("event=\"worker.operation.rejected\""),
            "{stderr}"
        );
        assert!(
            errors[1].contains("rejection=\"gateway-store-retention\""),
            "{stderr}"
        );
    }
    assert!(!socket.exists());
    drop(callback);
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
