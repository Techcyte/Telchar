//! Tests worker contracts and failure boundaries, including workload environment.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::thread;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;
use telchar::nomad::protocol::{
    Authentication, AuthenticationProof, BuildSpecification, Frame, FrameKind, InputManifest,
    NamedOutput, PathManifestEntry, ProtocolLimits, decode_metadata, encode_metadata, read_frame,
    write_frame,
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
    assert_eq!(stderr.contains("/nix/store/"), level == "debug", "{stderr}");
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
