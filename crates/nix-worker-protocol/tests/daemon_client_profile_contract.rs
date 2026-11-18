//! Tests daemon client profile contract contracts and failure boundaries, including integer.

use std::io::{self, Cursor, Read, Write};

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_ERROR, STDERR_LAST,
    WorkerClient, WorkerTrust, WorkerVersion,
};

struct ScriptedStream {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}

impl ScriptedStream {
    fn new(input: Vec<u8>) -> Self {
        Self {
            input: Cursor::new(input),
            output: Vec::new(),
        }
    }
}

impl Read for ScriptedStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.input.read(output)
    }
}

impl Write for ScriptedStream {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn integer(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn byte_string(output: &mut Vec<u8>, value: &[u8]) {
    integer(output, value.len() as u64);
    output.extend_from_slice(value);
    output.resize(output.len() + (8 - value.len() % 8) % 8, 0);
}

fn handshake_response(version: WorkerVersion, trust: Option<u64>) -> Vec<u8> {
    let mut response = Vec::new();
    integer(&mut response, SERVER_WORKER_MAGIC);
    integer(&mut response, version.to_wire());
    if version >= LATEST_WORKER_VERSION {
        integer(&mut response, 0);
    }
    if version >= WorkerVersion::new(1, 33) {
        byte_string(&mut response, b"2.34.8");
    }
    if version >= WorkerVersion::new(1, 35) {
        integer(&mut response, trust.expect("modern handshake has trust"));
    }
    integer(&mut response, STDERR_LAST);
    response
}

fn expected_handshake(version: WorkerVersion) -> Vec<u8> {
    let mut expected = Vec::new();
    integer(&mut expected, CLIENT_WORKER_MAGIC);
    integer(&mut expected, LATEST_WORKER_VERSION.to_wire());
    if version >= LATEST_WORKER_VERSION {
        integer(&mut expected, 0);
    }
    integer(&mut expected, 0);
    integer(&mut expected, 0);
    expected
}

#[test]
fn modern_daemon_handshake_returns_typed_profile() {
    let client = WorkerClient::connect(ScriptedStream::new(handshake_response(
        LATEST_WORKER_VERSION,
        Some(1),
    )))
    .expect("modern daemon handshake succeeds");

    assert_eq!(client.profile().version, LATEST_WORKER_VERSION);
    assert_eq!(client.profile().trust, WorkerTrust::Trusted);
    assert!(client.profile().capabilities.root_registration);
    assert_eq!(
        client.into_inner().output,
        expected_handshake(LATEST_WORKER_VERSION)
    );
}

#[test]
fn negotiation_caps_daemon_version_at_client_maximum() {
    let daemon_version = WorkerVersion::new(1, 39);
    let client = WorkerClient::connect(ScriptedStream::new(handshake_response(
        daemon_version,
        Some(2),
    )))
    .expect("newer daemon is capped at client maximum");

    assert_eq!(client.profile().version, LATEST_WORKER_VERSION);
    assert_eq!(client.profile().trust, WorkerTrust::Untrusted);
    assert_eq!(
        client.into_inner().output,
        expected_handshake(LATEST_WORKER_VERSION)
    );
}

#[test]
fn recent_daemon_profiles_are_accepted() {
    for (version, trust) in [
        (WorkerVersion::new(1, 35), WorkerTrust::Trusted),
        (WorkerVersion::new(1, 37), WorkerTrust::Trusted),
        (LATEST_WORKER_VERSION, WorkerTrust::Trusted),
    ] {
        let client =
            WorkerClient::connect(ScriptedStream::new(handshake_response(version, Some(1))))
                .expect("recent daemon handshake succeeds");

        assert_eq!(client.profile().version, version);
        assert_eq!(client.profile().trust, trust);
        assert!(client.profile().capabilities.root_registration);
        assert_eq!(client.into_inner().output, expected_handshake(version));
    }
}

#[test]
fn below_minimum_and_wrong_major_are_rejected() {
    for version in [WorkerVersion::new(1, 34), WorkerVersion::new(2, 38)] {
        let error =
            WorkerClient::connect(ScriptedStream::new(handshake_response(version, Some(1))))
                .err()
                .expect("unsupported daemon profile is rejected");

        assert_eq!(error.to_string(), "Nix daemon operation failed");
    }
}

#[test]
fn invalid_trust_value_is_rejected() {
    let error = WorkerClient::connect(ScriptedStream::new(handshake_response(
        LATEST_WORKER_VERSION,
        Some(3),
    )))
    .err()
    .expect("invalid trust value is rejected");

    assert_eq!(error.to_string(), "Nix daemon operation failed");
}

#[test]
fn oversized_post_handshake_metadata_is_rejected_before_allocation() {
    let mut response = Vec::new();
    integer(&mut response, SERVER_WORKER_MAGIC);
    integer(&mut response, LATEST_WORKER_VERSION.to_wire());
    integer(&mut response, 0);
    integer(&mut response, 1025);

    let error = WorkerClient::connect(ScriptedStream::new(response))
        .err()
        .expect("oversized daemon version is rejected before allocation");

    assert_eq!(error.to_string(), "Nix daemon operation failed");
}

#[test]
fn truncated_handshake_fields_fail_closed() {
    for response in [vec![], SERVER_WORKER_MAGIC.to_le_bytes().to_vec(), {
        let mut response = handshake_response(LATEST_WORKER_VERSION, Some(1));
        response.pop();
        response
    }] {
        let error = WorkerClient::connect(ScriptedStream::new(response))
            .err()
            .expect("truncated handshake is rejected");

        assert!(matches!(
            error.kind(),
            io::ErrorKind::UnexpectedEof | io::ErrorKind::Other
        ));
    }
}

#[test]
fn daemon_error_is_bounded_and_redacted() {
    let mut response = handshake_response(LATEST_WORKER_VERSION, Some(1));
    response.pop();
    response.extend_from_slice(&STDERR_ERROR.to_le_bytes());
    byte_string(&mut response, b"sensitive daemon diagnostic");
    integer(&mut response, 1);
    byte_string(&mut response, b"daemon");
    byte_string(&mut response, b"sensitive message");
    integer(&mut response, 0);
    integer(&mut response, 0);

    let error = WorkerClient::connect(ScriptedStream::new(response))
        .err()
        .expect("daemon startup error is redacted");

    assert_eq!(error.to_string(), "Nix daemon operation failed");
    assert!(!error.to_string().contains("sensitive"));
}

#[test]
fn malformed_feature_padding_is_rejected() {
    let mut response = Vec::new();
    integer(&mut response, SERVER_WORKER_MAGIC);
    integer(&mut response, LATEST_WORKER_VERSION.to_wire());
    integer(&mut response, 1);
    integer(&mut response, 1);
    response.push(b'x');
    response.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1]);

    let error = WorkerClient::connect(ScriptedStream::new(response))
        .err()
        .expect("nonzero feature padding is rejected");

    assert_eq!(error.to_string(), "Nix daemon operation failed");
}

#[test]
fn profile_does_not_retain_raw_daemon_metadata() {
    let client = WorkerClient::connect(ScriptedStream::new(handshake_response(
        LATEST_WORKER_VERSION,
        Some(1),
    )))
    .expect("modern daemon handshake succeeds");

    assert_eq!(
        std::mem::size_of_val(client.profile()),
        std::mem::size_of::<WorkerVersion>()
            + std::mem::size_of::<WorkerTrust>()
            + std::mem::size_of_val(&client.profile().capabilities)
    );
}

#[test]
fn add_signatures_exact_bytes_across_supported_profiles_without_trust_gate() {
    let path = b"/nix/store/00000000000000000000000000000000-output";
    for minor in 35..=38 {
        let version = WorkerVersion::new(1, minor);
        let mut response = handshake_response(version, Some(2));
        integer(&mut response, STDERR_LAST);
        integer(&mut response, 1);
        let mut client = WorkerClient::connect(ScriptedStream::new(response)).unwrap();
        client
            .add_signatures(path, &[b"builder:signature".to_vec()])
            .unwrap();
        let mut expected = expected_handshake(version);
        integer(&mut expected, 37);
        byte_string(&mut expected, path);
        integer(&mut expected, 1);
        byte_string(&mut expected, b"builder:signature");
        assert_eq!(client.into_inner().output, expected);
    }
}

#[test]
fn add_signatures_validates_before_writing_and_requires_complete_ack() {
    let version = LATEST_WORKER_VERSION;
    let mut client =
        WorkerClient::connect(ScriptedStream::new(handshake_response(version, Some(1)))).unwrap();
    assert!(client.add_signatures(b"bad-path", &[]).is_err());
    assert!(
        client
            .add_signatures(
                b"/nix/store/00000000000000000000000000000000-output",
                &[vec![b'x'; 16385]]
            )
            .is_err()
    );
    assert_eq!(client.into_inner().output, expected_handshake(version));
    for ack in [vec![], vec![1], 0u64.to_le_bytes().to_vec()] {
        let mut response = handshake_response(version, Some(1));
        integer(&mut response, STDERR_LAST);
        response.extend(ack);
        let mut client = WorkerClient::connect(ScriptedStream::new(response)).unwrap();
        assert!(
            client
                .add_signatures(b"/nix/store/00000000000000000000000000000000-output", &[])
                .is_err()
        );
    }
}
