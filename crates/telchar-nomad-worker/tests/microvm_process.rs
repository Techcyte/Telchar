use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn microvm_worker_bounds_manifest_wait_on_verified_wss() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let directory =
        std::env::temp_dir().join(format!("telchar-microvm-wss-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let binding = directory.join("execution.json");
    let trust = directory.join("trust.der");
    std::fs::write(&trust, include_bytes!("fixtures/microvm-cert.der")).unwrap();
    let bearer = "s".repeat(43);
    std::fs::write(
        &binding,
        serde_json::to_vec(&serde_json::json!({
            "endpoint": format!("wss://localhost:{}/microvm", address.port()),
            "address": address.to_string(), "intent_id": "intent", "provider_id": "microvm-exact",
            "bearer": bearer, "lifetime_seconds": 1
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&binding, std::fs::Permissions::from_mode(0o600)).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let tls = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![include_bytes!("fixtures/microvm-cert.der").to_vec().into()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(
                include_bytes!("fixtures/microvm-key.der").to_vec().into(),
            ),
        )
        .unwrap();
        let tls = rustls::StreamOwned::new(
            rustls::ServerConnection::new(Arc::new(tls)).unwrap(),
            stream,
        );
        let mut socket = tungstenite::accept_hdr(
            tls,
            |_: &tungstenite::handshake::server::Request,
             mut response: tungstenite::handshake::server::Response| {
                response.headers_mut().insert(
                    "sec-websocket-protocol",
                    "telchar-microvm-v1".parse().unwrap(),
                );
                Ok(response)
            },
        )
        .unwrap();
        let bootstrap: serde_json::Value =
            serde_json::from_slice(&socket.read().unwrap().into_data()).unwrap();
        assert_eq!(
            bootstrap,
            serde_json::json!({"intent_id":"intent", "provider_id":"microvm-exact", "bearer":bearer})
        );
        assert!(
            socket.read().is_err(),
            "deadline must disconnect silent manifest peer"
        );
    });
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_telchar-microvm-worker"))
        .env_clear()
        .arg(&binding)
        .arg(&trust)
        .arg("unix:///definitely-missing-nix.sock")
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    server.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"MicroVM worker failed\n");
    assert!(
        elapsed < Duration::from_secs(2),
        "execution deadline: {elapsed:?}"
    );
}

#[test]
fn microvm_worker_rejects_unprotected_bootstrap_without_secret_diagnostics() {
    let directory =
        std::env::temp_dir().join(format!("telchar-microvm-process-{}", std::process::id()));
    std::fs::create_dir(&directory).expect("private directory");
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let bootstrap = directory.join("execution.json");
    std::fs::write(&bootstrap, b"secret-runtime-capability").unwrap();
    std::fs::set_permissions(&bootstrap, std::fs::Permissions::from_mode(0o644)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_telchar-microvm-worker"))
        .arg(&bootstrap)
        .arg("unused-trust.der")
        .arg("unix:///definitely-missing-nix.sock")
        .output()
        .expect("worker runs");
    std::fs::remove_dir_all(&directory).unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "MicroVM worker failed\n"
    );
}
