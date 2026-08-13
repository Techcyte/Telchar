use std::collections::BTreeMap;
use std::net::TcpListener;
use std::thread;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::json;
use telchar::nomad_transfer_protocol::{
    decode_metadata, encode_metadata, read_frame, write_frame, Authentication, AuthenticationProof,
    BuildSpecification, Frame, FrameKind, InputManifest, NamedOutput, PathManifestEntry,
    ProtocolLimits,
};
use telchar_nomad_worker::{authenticate, receive_manifest, WorkerConfig};

fn workload_environment(endpoint: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("TELCHAR_TRANSFER_ENDPOINT".to_owned(), endpoint.to_owned()),
        ("TELCHAR_NIX_STORE_URI".to_owned(), "daemon".to_owned()),
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
        ("NOMAD_TOKEN".to_owned(), "jwt".to_owned()),
    ])
}

#[test]
fn parses_exact_workload_identity_environment() {
    let environment = workload_environment("ws://127.0.0.1:1234/callback");
    let config = WorkerConfig::from_lookup(|name| environment.get(name).cloned())
        .expect("worker environment parses");

    assert_eq!(config.store_uri(), "daemon");
    assert_eq!(config.endpoint().as_str(), "ws://127.0.0.1:1234/callback");
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
        ("TELCHAR_NIX_STORE_URI".to_owned(), "daemon".to_owned()),
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
    let expected = manifest();
    let sent = expected.clone();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("callback accepted");
        let mut socket = tungstenite::accept_hdr(stream, select_protocol).expect("socket accepts");
        let _ = socket.read().expect("authentication reads");
        let metadata = encode_metadata(&sent, 64 * 1024).expect("manifest encodes");
        let mut body = Vec::new();
        write_frame(
            &mut body,
            &Frame::new(FrameKind::InputManifest, metadata, vec![]),
            ProtocolLimits::new(64 * 1024, 0),
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
