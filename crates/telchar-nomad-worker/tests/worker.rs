//! Tests worker contracts and failure boundaries, including workload environment.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::thread;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;
use telchar::nomad::protocol::{
    Authentication, AuthenticationProof, BuildSpecification, Frame, FrameKind, InputManifest,
    NamedOutput, PathManifestEntry, PathSet, ProtocolLimits, decode_metadata, encode_metadata,
    read_frame, write_frame,
};
use telchar_nomad_worker::{WorkerConfig, authenticate, receive_manifest};

fn workload_environment(endpoint: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("TELCHAR_TRANSFER_ENDPOINT".to_owned(), endpoint.to_owned()),
        (
            "TELCHAR_NIX_STORE_URI".to_owned(),
            "unix:///nix/var/nix/daemon-socket/socket".to_owned(),
        ),
        (
            "TELCHAR_TRANSFER_CHUNK_BYTES".to_owned(),
            "262144".to_owned(),
        ),
        (
            "TELCHAR_MAXIMUM_MANIFEST_BYTES".to_owned(),
            "8388608".to_owned(),
        ),
        (
            "TELCHAR_TRANSFER_IDLE_TIMEOUT_SECONDS".to_owned(),
            "30".to_owned(),
        ),
        ("TELCHAR_SETUP_TIMEOUT_SECONDS".to_owned(), "300".to_owned()),
        (
            "TELCHAR_OUTPUT_COLLECTION_TIMEOUT_SECONDS".to_owned(),
            "300".to_owned(),
        ),
        (
            "TELCHAR_MAXIMUM_CONNECTION_LIFETIME_SECONDS".to_owned(),
            "3600".to_owned(),
        ),
        (
            "TELCHAR_MAXIMUM_DIAGNOSTIC_BYTES".to_owned(),
            "65536".to_owned(),
        ),
        (
            "TELCHAR_TRANSFER_AUTHENTICATION".to_owned(),
            "workload-identity".to_owned(),
        ),
        ("TELCHAR_BACKEND".to_owned(), "nomad-primary".to_owned()),
        ("TELCHAR_NAMESPACE".to_owned(), "telchar".to_owned()),
        ("TELCHAR_JOB_ID".to_owned(), "job-1".to_owned()),
        (
            "TELCHAR_SHARED_BUILD_DIGEST".to_owned(),
            "digest-1".to_owned(),
        ),
        ("TELCHAR_TASK".to_owned(), "build".to_owned()),
        ("NOMAD_NAMESPACE".to_owned(), "telchar".to_owned()),
        ("NOMAD_JOB_ID".to_owned(), "job-1".to_owned()),
        ("NOMAD_ALLOC_ID".to_owned(), "allocation-1".to_owned()),
        ("NOMAD_TASK_NAME".to_owned(), "build".to_owned()),
        ("NOMAD_TOKEN_telchar_transfer".to_owned(), "jwt".to_owned()),
        (
            "TRACEPARENT".to_owned(),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_owned(),
        ),
        ("TRACESTATE".to_owned(), "vendor=value".to_owned()),
    ])
}

#[test]
fn worker_reports_configuration_failure_without_environment_values() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_telchar-nomad-worker"))
        .env_clear()
        .env("TELCHAR_TRANSFER_ENDPOINT", "private-marker://secret")
        .output()
        .expect("worker runs");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("event=\"worker.phase.started\" phase=\"configuration\""),
        "{stderr}"
    );
    assert!(
        stderr.contains("event=\"worker.phase.failed\" phase=\"configuration\""),
        "{stderr}"
    );
    assert!(stderr.contains(" INFO "), "{stderr}");
    assert!(stderr.contains(" ERROR "), "{stderr}");
    assert!(
        stderr.contains("worker callback endpoint is invalid"),
        "{stderr}"
    );
    assert!(stderr.contains("error_kind=InvalidInput"), "{stderr}");
    assert!(!stderr.contains("private-marker"), "{stderr}");
    assert!(!stderr.contains("phase=manifest"), "{stderr}");
}

#[test]
fn worker_rejects_untrusted_callback_without_panicking() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    listener
        .set_nonblocking(true)
        .expect("listener is nonblocking");
    let endpoint = format!("wss://{}/callback", listener.local_addr().expect("address"));
    let mut environment = workload_environment(&endpoint);
    environment.insert("TELCHAR_SETUP_TIMEOUT_SECONDS".to_owned(), "1".to_owned());
    let server = thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < deadline, "worker must connect");
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("callback accept failed: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .expect("read timeout");
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(3)))
            .expect("write timeout");
        let config = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![include_bytes!("fixtures/callback-cert.der").to_vec().into()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(
                include_bytes!("fixtures/callback-key.der").to_vec().into(),
            ),
        )
        .expect("server certificate");
        let mut tls = rustls::StreamOwned::new(
            rustls::ServerConnection::new(std::sync::Arc::new(config)).expect("TLS connection"),
            stream,
        );
        tls.conn
            .complete_io(&mut tls.sock)
            .expect_err("untrusted certificate is rejected")
    });
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_telchar-nomad-worker"))
        .env_clear()
        .envs(environment)
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
        .output()
        .expect("worker runs");
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(
        stderr.contains("worker startup failed at connect"),
        "{stderr}"
    );
    assert!(!stderr.contains("worker.manifest.received"), "{stderr}");
    assert!(!stderr.contains("jwt"), "{stderr}");
    let error = server.join().expect("server joins");
    assert!(
        matches!(
            error
                .get_ref()
                .and_then(|error| error.downcast_ref::<rustls::Error>()),
            Some(rustls::Error::AlertReceived(
                rustls::AlertDescription::UnknownCA
            ))
        ),
        "{error:?}"
    );
}

#[test]
fn setup_deadline_bounds_silent_tls_and_trickling_websocket_peers() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    let _ = rustls::crypto::ring::default_provider().install_default();
    for (scheme, trickling) in [("ws", false), ("ws", true), ("wss", false)] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let endpoint = format!("{scheme}://{}/callback", listener.local_addr().unwrap());
        let mut environment = workload_environment(&endpoint);
        environment.insert("TELCHAR_SETUP_TIMEOUT_SECONDS".into(), "1".into());
        let config = WorkerConfig::from_lookup(|key| environment.get(key).cloned()).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut bytes = [0; 4096];
            let mut response =
                b"HTTP/1.1 101 Switching Protocols\r\nX-Padding: indefinitely-incomplete".iter();
            while Instant::now() < deadline {
                match stream.read(&mut bytes) {
                    Ok(0) => return true,
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(_) => return true,
                }
                // Activity must not restart the setup budget. TLS deliberately stays silent.
                if trickling {
                    let byte = response.next().copied().unwrap_or(b'a');
                    if stream.write_all(&[byte]).is_err() {
                        return true;
                    }
                }
            }
            false
        });
        let started = Instant::now();
        let result = telchar_nomad_worker::connect(&config);
        let elapsed = started.elapsed();
        let peer_closed = server.join().unwrap();
        assert!(result.is_err(), "{scheme} setup must fail");
        assert!(
            elapsed < Duration::from_millis(1800),
            "{scheme} setup took {elapsed:?}"
        );
        assert!(
            peer_closed,
            "{scheme} setup must close the abandoned socket"
        );
    }
}

#[test]
fn setup_retries_share_the_original_deadline() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}/callback", listener.local_addr().unwrap());
    let mut environment = workload_environment(&endpoint);
    environment.insert("TELCHAR_SETUP_TIMEOUT_SECONDS".into(), "1".into());
    let config = WorkerConfig::from_lookup(|key| environment.get(key).cloned()).unwrap();
    let server = thread::spawn(move || {
        let (mut first, _) = listener.accept().unwrap();
        first
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = [0; 4096];
        assert!(first.read(&mut bytes).unwrap() > 0);
        thread::sleep(Duration::from_millis(600));
        first
            .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        drop(first);
        let (mut retry, _) = listener.accept().unwrap();
        retry
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        loop {
            match retry.read(&mut bytes) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) => panic!("retry must close at setup deadline: {error}"),
            }
        }
    });
    let started = Instant::now();
    assert!(telchar_nomad_worker::connect(&config).is_err());
    assert!(started.elapsed() < Duration::from_millis(1500));
    server.join().unwrap();
}

#[test]
fn setup_preserves_redirects_and_explicit_loopback_plaintext() {
    use std::io::{Read, Write};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}/callback", listener.local_addr().unwrap());
    let location = format!(
        "ws://localhost:{}/redirected",
        target.local_addr().unwrap().port()
    );
    let environment = workload_environment(&endpoint);
    let config = WorkerConfig::from_lookup(|key| environment.get(key).cloned()).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut bytes = [0; 4096];
        assert!(stream.read(&mut bytes).unwrap() > 0);
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
        )
        .unwrap();
        drop(stream);
        let (stream, _) = target.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut socket = tungstenite::accept_hdr(stream, select_protocol).unwrap();
        let message = socket.read().unwrap();
        assert!(message.is_binary(), "authentication follows the redirect");
    });
    let socket = telchar_nomad_worker::connect(&config).unwrap();
    drop(socket);
    server.join().unwrap();
}

#[test]
fn worker_reports_manifest_and_store_failure_without_payloads() {
    for level in ["info", "debug"] {
        check_manifest_failure(level);
    }
}

fn check_manifest_failure(level: &str) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let endpoint = format!("ws://{}/callback", listener.local_addr().expect("address"));
    let mut environment = workload_environment(&endpoint);
    environment.insert(
        "TELCHAR_NIX_STORE_URI".to_owned(),
        "unix:///nonexistent-private-marker/socket".to_owned(),
    );
    environment.insert(
        "NOMAD_TOKEN_telchar_transfer".to_owned(),
        "private-token-marker".to_owned(),
    );
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("callback accepted");
        let mut socket = tungstenite::accept_hdr(stream, select_protocol).expect("socket accepts");
        let _ = socket.read().expect("authentication reads");
        let mut sent = manifest();
        sent.build
            .environment
            .push((b"SECRET".to_vec(), b"private-build-marker".to_vec()));
        let metadata = encode_metadata(&sent, 8 * 1024 * 1024).expect("manifest encodes");
        let mut body = Vec::new();
        write_frame(
            &mut body,
            &Frame::new(FrameKind::InputManifest, metadata, vec![]),
            ProtocolLimits::new(8 * 1024 * 1024, 0),
        )
        .expect("frame writes");
        socket
            .send(tungstenite::Message::Binary(body.into()))
            .expect("manifest sends");
    });
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_telchar-nomad-worker"))
        .env_clear()
        .envs(environment)
        .env("RUST_LOG", format!("info,telchar_nomad_worker={level}"))
        .output()
        .expect("worker runs");
    server.join().expect("server joins");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("event=\"worker.phase.completed\" phase=\"configuration\""),
        "{stderr}"
    );
    assert!(
        stderr.contains("event=\"worker.phase.completed\" phase=\"manifest\""),
        "{stderr}"
    );
    assert!(
        stderr.contains("event=\"worker.manifest.received\" input_count=1 output_count=1"),
        "{stderr}"
    );
    assert!(
        stderr.contains("event=\"worker.phase.failed\" phase=\"resolve-inputs\""),
        "{stderr}"
    );
    assert!(stderr.contains("elapsed_ms="), "{stderr}");
    assert!(
        stderr.contains("event=\"worker.callback.connected\""),
        "{stderr}"
    );
    assert!(
        stderr.contains("event=\"worker.store.connecting\" operation=\"resolve-inputs\""),
        "{stderr}"
    );
    for secret in [
        "private-marker",
        "private-token-marker",
        "private-build-marker",
    ] {
        assert!(!stderr.contains(secret), "{stderr}");
    }
    assert!(!stderr.contains("phase=\"build\""), "{stderr}");
    let identity = stderr
        .lines()
        .find(|line| line.contains("event=\"worker.derivation.received\""))
        .expect("worker reports build identity");
    assert!(identity.contains(" INFO "), "{stderr}");
    assert!(identity.contains(&manifest().derivation_path), "{stderr}");
    let failure = stderr
        .lines()
        .find(|line| line.contains("event=\"worker.failed\""))
        .expect("worker reports terminal failure");
    assert!(failure.contains(" ERROR "), "{stderr}");
    assert!(
        failure.contains("reason=worker Nix store connection failed"),
        "{stderr}"
    );
}

#[test]
fn parses_exact_workload_identity_environment() {
    let environment = workload_environment("ws://127.0.0.1:1234/callback");
    let config = WorkerConfig::from_lookup(|name| environment.get(name).cloned())
        .expect("worker environment parses");

    assert_eq!(
        config.store_uri(),
        "unix:///nix/var/nix/daemon-socket/socket"
    );
    assert_eq!(config.transfer_chunk_bytes(), 262_144);
    assert_eq!(config.maximum_manifest_bytes(), 8_388_608);
    assert_eq!(
        config.transfer_idle_timeout(),
        std::time::Duration::from_secs(30)
    );
    assert_eq!(config.setup_timeout(), std::time::Duration::from_secs(300));
    assert_eq!(
        config.output_collection_timeout(),
        std::time::Duration::from_secs(300)
    );
    assert_eq!(
        config.maximum_connection_lifetime(),
        std::time::Duration::from_secs(3600)
    );
    assert_eq!(config.endpoint().as_str(), "ws://127.0.0.1:1234/callback");
    assert_eq!(
        config
            .trace_context()
            .trace_id()
            .expect("trace ID exists")
            .to_string(),
        "4bf92f3577b34da6a3ce929d0e0e4736"
    );
    assert_eq!(
        config.authentication(),
        &Authentication {
            backend: "nomad-primary".to_owned(),
            namespace: "telchar".to_owned(),
            job_id: "job-1".to_owned(),
            allocation_id: "allocation-1".to_owned(),
            task: "build".to_owned(),
            shared_build_digest: "digest-1".to_owned(),
            proof: AuthenticationProof::WorkloadIdentity {
                token: "jwt".to_owned(),
            },
        }
    );
}

#[test]
fn derives_hmac_identity_only_from_signed_capability_and_nomad_environment() {
    let claims = json!({
        "version": 1,
        "key_id": "key-1",
        "backend": "nomad-primary",
        "namespace": "telchar",
        "job_id": "job-1",
        "shared_build_digest": "digest-1",
        "issued_at": 1,
        "expires_at": 4_000_000_000_u64,
        "nonce": "nonce-1",
        "request_key": URL_SAFE_NO_PAD.encode([7_u8; 32]),
    });
    let capability = format!(
        "{}.signature",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("claims encode"))
    );
    let environment = BTreeMap::from([
        (
            "TELCHAR_TRANSFER_ENDPOINT".to_owned(),
            "wss://gateway.example/callback".to_owned(),
        ),
        (
            "TELCHAR_NIX_STORE_URI".to_owned(),
            "unix:///nix/var/nix/daemon-socket/socket".to_owned(),
        ),
        (
            "TELCHAR_TRANSFER_CHUNK_BYTES".to_owned(),
            "262144".to_owned(),
        ),
        (
            "TELCHAR_MAXIMUM_MANIFEST_BYTES".to_owned(),
            "8388608".to_owned(),
        ),
        (
            "TELCHAR_TRANSFER_IDLE_TIMEOUT_SECONDS".to_owned(),
            "30".to_owned(),
        ),
        ("TELCHAR_SETUP_TIMEOUT_SECONDS".to_owned(), "300".to_owned()),
        (
            "TELCHAR_OUTPUT_COLLECTION_TIMEOUT_SECONDS".to_owned(),
            "300".to_owned(),
        ),
        (
            "TELCHAR_MAXIMUM_CONNECTION_LIFETIME_SECONDS".to_owned(),
            "3600".to_owned(),
        ),
        (
            "TELCHAR_MAXIMUM_DIAGNOSTIC_BYTES".to_owned(),
            "65536".to_owned(),
        ),
        (
            "TELCHAR_TRANSFER_AUTHENTICATION".to_owned(),
            "hmac".to_owned(),
        ),
        ("TELCHAR_TRANSFER_CAPABILITY".to_owned(), capability.clone()),
        ("NOMAD_NAMESPACE".to_owned(), "telchar".to_owned()),
        ("NOMAD_JOB_ID".to_owned(), "job-1".to_owned()),
        ("NOMAD_ALLOC_ID".to_owned(), "allocation-1".to_owned()),
        ("NOMAD_TASK_NAME".to_owned(), "build".to_owned()),
    ]);

    let config = WorkerConfig::from_lookup(|name| environment.get(name).cloned())
        .expect("HMAC worker environment parses");
    let AuthenticationProof::Hmac {
        capability: actual, ..
    } = &config.authentication().proof
    else {
        panic!("expected HMAC proof");
    };
    assert_eq!(actual.as_str(), capability);
    assert_eq!(config.authentication().backend, "nomad-primary");
    assert_eq!(config.authentication().allocation_id, "allocation-1");
}

#[test]
fn rejects_store_uri_before_callback_connection() {
    for uri in [
        "daemon",
        "/nix/var/nix/daemon-socket/socket",
        "unix://relative",
        "unix:///socket?hidden",
    ] {
        let mut environment = workload_environment("ws://127.0.0.1:1234/callback");
        environment.insert("TELCHAR_NIX_STORE_URI".to_owned(), uri.to_owned());
        let error = WorkerConfig::from_lookup(|name| environment.get(name).cloned()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "worker store URI must be unix:///absolute/socket without query or fragment"
        );
    }
}

#[test]
fn rejects_missing_or_mismatched_identity_environment() {
    let mut missing = workload_environment("ws://127.0.0.1:1234/callback");
    missing.remove("NOMAD_ALLOC_ID");
    assert!(WorkerConfig::from_lookup(|name| missing.get(name).cloned()).is_err());

    let mut mismatched = workload_environment("ws://127.0.0.1:1234/callback");
    mismatched.insert("NOMAD_TASK_NAME".to_owned(), "foreign".to_owned());
    assert!(WorkerConfig::from_lookup(|name| mismatched.get(name).cloned()).is_err());

    let mut invalid_endpoint = workload_environment("file:///tmp/callback");
    assert!(WorkerConfig::from_lookup(|name| invalid_endpoint.get(name).cloned()).is_err());
    invalid_endpoint.insert(
        "TELCHAR_TRANSFER_ENDPOINT".to_owned(),
        "ws://gateway".to_owned(),
    );
}

#[allow(clippy::result_large_err)]
fn select_protocol(
    request: &tungstenite::handshake::server::Request,
    mut response: tungstenite::handshake::server::Response,
) -> Result<tungstenite::handshake::server::Response, tungstenite::handshake::server::ErrorResponse>
{
    assert_eq!(
        request.headers().get("sec-websocket-protocol"),
        Some(&tungstenite::http::HeaderValue::from_static(
            "telchar-nomad-transfer-v1"
        ))
    );
    response.headers_mut().insert(
        "sec-websocket-protocol",
        tungstenite::http::HeaderValue::from_static("telchar-nomad-transfer-v1"),
    );
    Ok(response)
}

#[test]
fn receives_manifest_on_execution_authenticated_connection() {
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
        let endpoint = format!("ws://{}/fixture", listener.local_addr().expect("address"));
        let expected = manifest();
        let sent = expected.clone();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("connection accepts");
            let mut socket = tungstenite::accept(stream).expect("WebSocket accepts");
            let mut body = Vec::new();
            write_frame(
                &mut body,
                &Frame::new(
                    FrameKind::InputManifest,
                    encode_metadata(&sent, 8 * 1024 * 1024).expect("manifest encodes"),
                    vec![],
                ),
                ProtocolLimits::new(8 * 1024 * 1024, 0),
            )
            .expect("frame writes");
            socket
                .send(tungstenite::Message::Binary(body.into()))
                .expect("manifest sends");
        });
        let (socket, _) = tungstenite::connect(endpoint).expect("connection opens");
        let session = telchar_nomad_worker::receive_authenticated_manifest(
            socket,
            8 * 1024 * 1024,
            262144,
            std::time::Duration::from_secs(60),
        )
        .expect("execution-authenticated session receives manifest without Nomad identity");
        assert_eq!(session.manifest(), &expected);
        server.join().expect("server joins");
    }
}

#[test]
fn authenticated_session_rejects_unbounded_limits_before_reading() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let address = listener.local_addr().expect("address");
    let server = thread::spawn(move || {
        for _ in 0..4 {
            let (stream, _) = listener.accept().expect("connection accepts");
            let _socket = tungstenite::accept(stream).expect("WebSocket accepts");
        }
    });
    for (metadata, chunk, lifetime) in [
        (0, 1, 1),
        (64 * 1024 * 1024 + 1, 1, 1),
        (1, 1024 * 1024 + 1, 1),
        (1, 1, 0),
    ] {
        let (socket, _) = tungstenite::connect(format!("ws://{address}")).expect("connects");
        let error = telchar_nomad_worker::receive_authenticated_manifest(
            socket,
            metadata,
            chunk,
            std::time::Duration::from_secs(lifetime),
        )
        .err()
        .expect("invalid limits rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
    server.join().expect("server joins");
}

fn manifest() -> InputManifest {
    let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-build.drv".to_owned();
    let input = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input".to_owned();
    let output = "/nix/store/cccccccccccccccccccccccccccccccc-output".to_owned();
    InputManifest {
        derivation_path: derivation.clone(),
        build: BuildSpecification {
            derivation_path: derivation.into_bytes(),
            outputs: vec![NamedOutput {
                name: b"out".to_vec(),
                path: output.clone().into_bytes(),
                hash_algorithm: Vec::new(),
                hash: Vec::new(),
            }],
            input_sources: vec![input.clone().into_bytes()],
            system: "x86_64-linux".to_owned(),
            required_system_features: vec![],
            builder: b"/bin/sh".to_vec(),
            arguments: vec![b"-e".to_vec()],
            environment: vec![
                (b"system".to_vec(), b"x86_64-linux".to_vec()),
                (b"builder".to_vec(), b"/bin/sh".to_vec()),
                (b"name".to_vec(), b"build".to_vec()),
                (b"out".to_vec(), output.into_bytes()),
            ],
        },
        paths: vec![PathManifestEntry {
            path: input,
            nar_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
            nar_size: 42,
            references: vec![],
            deriver: None,
            content_address: None,
        }],
        outputs: vec!["/nix/store/cccccccccccccccccccccccccccccccc-output".to_owned()],
    }
}

#[test]
fn receives_exact_bounded_manifest_after_authentication() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let endpoint = format!("ws://{}/callback", listener.local_addr().expect("address"));
    let environment = workload_environment(&endpoint);
    let config = WorkerConfig::from_lookup(|name| environment.get(name).cloned())
        .expect("worker environment parses");
    let mut expected = manifest();
    for index in 0..2_534 {
        expected.paths.push(PathManifestEntry {
            path: format!("/nix/store/00000000000000000000000000000000-input-{index}"),
            nar_hash: "0".repeat(64),
            nar_size: 1,
            references: vec![],
            deriver: None,
            content_address: None,
        });
    }
    let sent = expected.clone();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("callback accepted");
        let observed_traceparent = std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_traceparent = std::sync::Arc::clone(&observed_traceparent);
        #[allow(clippy::result_large_err)]
        let capture_trace_context = move |request: &tungstenite::handshake::server::Request,
                                          response| {
            *captured_traceparent
                .lock()
                .expect("trace header lock holds") = request
                .headers()
                .get("traceparent")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            select_protocol(request, response)
        };
        let mut socket =
            tungstenite::accept_hdr(stream, capture_trace_context).expect("socket accepts");
        assert_eq!(
            observed_traceparent
                .lock()
                .expect("trace header lock holds")
                .as_deref(),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
        let _ = socket.read().expect("authentication reads");
        let metadata = encode_metadata(&sent, 8 * 1024 * 1024).expect("manifest encodes");
        let mut body = Vec::new();
        write_frame(
            &mut body,
            &Frame::new(FrameKind::InputManifest, metadata, vec![]),
            ProtocolLimits::new(8 * 1024 * 1024, 0),
        )
        .expect("manifest frame writes");
        socket
            .send(tungstenite::Message::Binary(body.into()))
            .expect("manifest sends");
    });

    let received = receive_manifest(&config).expect("worker receives manifest");
    assert_eq!(received.manifest(), &expected);
    server.join().expect("server joins");
}

#[test]
fn waits_for_callback_listener_during_setup() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let address = listener.local_addr().expect("address");
    drop(listener);
    let endpoint = format!("ws://{address}/callback");
    let mut environment = workload_environment(&endpoint);
    environment.insert("TELCHAR_SETUP_TIMEOUT_SECONDS".to_owned(), "2".to_owned());
    let config = WorkerConfig::from_lookup(|name| environment.get(name).cloned())
        .expect("worker environment parses");
    let server = thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(200));
        let listener = TcpListener::bind(address).expect("delayed listener binds");
        let (stream, _) = listener.accept().expect("callback accepted");
        let mut socket = tungstenite::accept_hdr(stream, select_protocol).expect("socket accepts");
        let _ = socket.read().expect("authentication reads");
    });

    authenticate(&config).expect("callback authentication waits for listener");
    server.join().expect("server joins");
}

#[test]
fn sends_one_bounded_authentication_frame_over_websocket() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let endpoint = format!("ws://{}/callback", listener.local_addr().expect("address"));
    let environment = workload_environment(&endpoint);
    let config = WorkerConfig::from_lookup(|name| environment.get(name).cloned())
        .expect("worker environment parses");
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("callback accepted");
        let mut socket =
            tungstenite::accept_hdr(stream, select_protocol).expect("WebSocket accepts");
        let tungstenite::Message::Binary(body) = socket.read().expect("message reads") else {
            panic!("expected binary authentication frame");
        };
        let frame =
            read_frame(&mut body.as_ref(), ProtocolLimits::new(4096, 0)).expect("frame reads");
        assert_eq!(frame.kind(), FrameKind::Authenticate);
        decode_metadata::<Authentication>(frame.metadata(), 4096).expect("authentication decodes")
    });

    authenticate(&config).expect("callback authentication succeeds");
    assert_eq!(
        server.join().expect("server joins"),
        config.authentication().clone()
    );
}

// The fake daemon speaks the real worker wire protocol; it never touches a Nix store.
fn resolve_inputs_with_daemon(
    path_count: usize,
    valid_indices: &[usize],
    invalid_response: Option<Vec<Vec<u8>>>,
) {
    use nix_worker_protocol::{
        CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_LAST,
    };
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    const QUERY_VALID_PATHS: u64 = 31;
    const IS_VALID_PATH: u64 = 1;

    fn integer(stream: &mut impl Read) -> u64 {
        let mut bytes = [0; 8];
        stream.read_exact(&mut bytes).unwrap();
        u64::from_le_bytes(bytes)
    }
    fn string(stream: &mut impl Read) -> Vec<u8> {
        let length = integer(stream) as usize;
        assert!(length <= 4096);
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).unwrap();
        let mut padding = [0; 7];
        stream
            .read_exact(&mut padding[..(8 - length % 8) % 8])
            .unwrap();
        bytes
    }
    fn write_integer(stream: &mut impl Write, value: u64) {
        stream.write_all(&value.to_le_bytes()).unwrap();
    }
    fn write_string(stream: &mut impl Write, value: &[u8]) {
        write_integer(stream, value.len() as u64);
        stream.write_all(value).unwrap();
        stream
            .write_all(&[0; 7][..(8 - value.len() % 8) % 8])
            .unwrap();
    }

    let mut manifest = manifest();
    let template = manifest.paths[0].clone();
    manifest.paths = (0..path_count)
        .map(|index| PathManifestEntry {
            path: format!("/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input-{index}"),
            ..template.clone()
        })
        .collect();
    manifest.build.input_sources = manifest
        .paths
        .iter()
        .map(|entry| entry.path.as_bytes().to_vec())
        .collect();
    let paths = manifest
        .paths
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    // Reverse response order to exercise set semantics, not positional matching.
    let valid = valid_indices
        .iter()
        .rev()
        .map(|&index| paths[index].clone())
        .collect::<Vec<_>>();
    let mut requested = paths
        .iter()
        .filter(|path| !valid.contains(path))
        .cloned()
        .collect::<Vec<_>>();
    requested.sort();
    let reject = invalid_response.is_some();
    let response = invalid_response
        .unwrap_or_else(|| valid.iter().map(|path| path.as_bytes().to_vec()).collect());
    let directory = tempfile::tempdir().unwrap();
    let socket_path = directory.path().join("daemon.sock");
    let daemon = UnixListener::bind(&socket_path).unwrap();
    let queried_paths = paths.clone();
    let daemon_thread = thread::spawn(move || {
        let (mut stream, _) = daemon.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(integer(&mut stream), CLIENT_WORKER_MAGIC);
        assert_eq!(integer(&mut stream), LATEST_WORKER_VERSION.to_wire());
        write_integer(&mut stream, SERVER_WORKER_MAGIC);
        write_integer(&mut stream, LATEST_WORKER_VERSION.to_wire());
        assert_eq!(integer(&mut stream), 0); // feature negotiation
        write_integer(&mut stream, 0);
        assert_eq!(integer(&mut stream), 0); // obsolete CPU affinity
        assert_eq!(integer(&mut stream), 0); // obsolete reserve space
        write_string(&mut stream, b"2.34.8");
        write_integer(&mut stream, 1); // trusted
        write_integer(&mut stream, STDERR_LAST);
        let mut queries = 0;
        let mut individual_queries = 0;
        loop {
            let mut operation = [0; 8];
            match stream.read_exact(&mut operation) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(error) => panic!("daemon operation read failed: {error}"),
            }
            let operation = u64::from_le_bytes(operation);
            if operation == QUERY_VALID_PATHS {
                queries += 1;
                let count = integer(&mut stream) as usize;
                assert!(count <= nix_worker_protocol::MAXIMUM_QUERY_VALID_PATHS);
                let actual = (0..count).map(|_| string(&mut stream)).collect::<Vec<_>>();
                assert_eq!(
                    actual,
                    queried_paths
                        .iter()
                        .map(|path| path.as_bytes().to_vec())
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    integer(&mut stream),
                    0,
                    "validity query must not substitute"
                );
                write_integer(&mut stream, STDERR_LAST);
                write_integer(&mut stream, response.len() as u64);
                for path in &response {
                    write_string(&mut stream, path);
                }
            } else if operation == IS_VALID_PATH {
                // Support the old production path so red failures measure behavior.
                individual_queries += 1;
                let path = string(&mut stream);
                write_integer(&mut stream, STDERR_LAST);
                write_integer(&mut stream, u64::from(response.contains(&path)));
            } else {
                panic!("unexpected daemon operation {operation}");
            }
        }
        (queries, individual_queries)
    });

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}/fixture", listener.local_addr().unwrap());
    let callback = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let mut bytes = Vec::new();
        write_frame(
            &mut bytes,
            &Frame::new(
                FrameKind::InputManifest,
                encode_metadata(&manifest, 8 * 1024 * 1024).unwrap(),
                vec![],
            ),
            ProtocolLimits::new(8 * 1024 * 1024, 0),
        )
        .unwrap();
        socket
            .send(tungstenite::Message::Binary(bytes.into()))
            .unwrap();
        let mut frames = Vec::new();
        loop {
            match socket.read() {
                Ok(message) => frames.push(
                    read_frame(
                        &mut message.into_data().as_ref(),
                        ProtocolLimits::new(8 * 1024 * 1024, 0),
                    )
                    .unwrap(),
                ),
                Err(tungstenite::Error::Protocol(
                    tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                )) => break,
                Err(error) => panic!("callback read failed: {error}"),
            }
        }
        frames
    });
    let (socket, _) = tungstenite::connect(endpoint).unwrap();
    let mut session = telchar_nomad_worker::receive_authenticated_manifest(
        socket,
        8 * 1024 * 1024,
        262144,
        Duration::from_secs(60),
    )
    .unwrap();
    let result = session.resolve_inputs(&format!("unix://{}", socket_path.display()));
    drop(session);
    let (queries, individual_queries) = daemon_thread.join().unwrap();
    let frames = callback.join().unwrap();
    println!(
        "resolve_inputs: paths={path_count} QueryValidPaths={queries} IsValidPath={individual_queries} rejected={reject}"
    );
    if reject {
        assert!(
            result.is_err(),
            "invalid daemon response must fail closed, got {result:?}"
        );
        assert!(
            frames.is_empty(),
            "invalid results must not be sent to the gateway"
        );
    } else {
        assert_eq!(result.unwrap().paths, requested);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].kind(), FrameKind::ValidPaths);
        let actual: PathSet = decode_metadata(frames[0].metadata(), 8 * 1024 * 1024).unwrap();
        assert_eq!(
            actual
                .paths
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            valid.into_iter().collect()
        );
        assert_eq!(frames[1].kind(), FrameKind::InputRequest);
        let actual: PathSet = decode_metadata(frames[1].metadata(), 8 * 1024 * 1024).unwrap();
        assert_eq!(actual.paths, requested);
    }
    assert_eq!(
        queries, 1,
        "resolve_inputs must issue one bounded QueryValidPaths"
    );
    assert_eq!(
        individual_queries, 0,
        "resolve_inputs must not issue per-path queries"
    );
}

#[test]
fn resolve_inputs_batches_all_warm_paths() {
    resolve_inputs_with_daemon(3, &[0, 1, 2], None);
}

#[test]
fn resolve_inputs_batches_mixed_paths() {
    resolve_inputs_with_daemon(3, &[0, 2], None);
}

#[test]
fn resolve_inputs_batches_no_warm_paths() {
    resolve_inputs_with_daemon(3, &[], None);
}

#[test]
fn resolve_inputs_batches_maximum_manifest() {
    resolve_inputs_with_daemon(
        nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES,
        &[],
        None,
    );
}

#[test]
fn resolve_inputs_rejects_foreign_valid_path() {
    resolve_inputs_with_daemon(
        3,
        &[],
        Some(vec![
            b"/nix/store/cccccccccccccccccccccccccccccccc-foreign".to_vec(),
        ]),
    );
}

#[test]
fn resolve_inputs_rejects_duplicate_valid_path() {
    let path = b"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input-0".to_vec();
    resolve_inputs_with_daemon(3, &[], Some(vec![path.clone(), path]));
}
