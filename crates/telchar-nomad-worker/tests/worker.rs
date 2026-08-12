use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::json;
use telchar::nomad_transfer_protocol::{
    decode_metadata, read_frame, Authentication, AuthenticationProof, FrameKind, ProtocolLimits,
};
use telchar_nomad_worker::{authenticate, WorkerConfig};

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
    let environment = workload_environment("http://127.0.0.1:1234/callback");
    let config = WorkerConfig::from_lookup(|name| environment.get(name).cloned())
        .expect("worker environment parses");

    assert_eq!(config.store_uri(), "daemon");
    assert_eq!(config.endpoint().as_str(), "http://127.0.0.1:1234/callback");
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
            "https://gateway.example/callback".to_owned(),
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
    let mut missing = workload_environment("http://127.0.0.1:1234/callback");
    missing.remove("NOMAD_ALLOC_ID");
    assert!(WorkerConfig::from_lookup(|name| missing.get(name).cloned()).is_err());

    let mut mismatched = workload_environment("http://127.0.0.1:1234/callback");
    mismatched.insert("NOMAD_TASK_NAME".to_owned(), "foreign".to_owned());
    assert!(WorkerConfig::from_lookup(|name| mismatched.get(name).cloned()).is_err());

    let mut invalid_endpoint = workload_environment("file:///tmp/callback");
    assert!(WorkerConfig::from_lookup(|name| invalid_endpoint.get(name).cloned()).is_err());
    invalid_endpoint.insert(
        "TELCHAR_TRANSFER_ENDPOINT".to_owned(),
        "http://gateway".to_owned(),
    );
}

#[test]
fn posts_one_bounded_authentication_frame() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let endpoint = format!(
        "http://{}/callback",
        listener.local_addr().expect("address")
    );
    let environment = workload_environment(&endpoint);
    let config = WorkerConfig::from_lookup(|name| environment.get(name).cloned())
        .expect("worker environment parses");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("callback accepted");
        let mut headers = Vec::new();
        let mut byte = [0_u8; 1];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).expect("header byte reads");
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).expect("headers are UTF-8");
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(str::to_owned)
            })
            .expect("content length exists")
            .parse::<usize>()
            .expect("content length parses");
        let mut body = vec![0_u8; content_length];
        stream.read_exact(&mut body).expect("body reads");
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("response writes");
        let frame =
            read_frame(&mut body.as_slice(), ProtocolLimits::new(4096, 0)).expect("frame reads");
        assert_eq!(frame.kind(), FrameKind::Authenticate);
        decode_metadata::<Authentication>(frame.metadata(), 4096).expect("authentication decodes")
    });

    authenticate(&config).expect("callback authentication succeeds");
    assert_eq!(
        server.join().expect("server joins"),
        config.authentication().clone()
    );
}
