//! Tests daemon path query contract contracts and failure boundaries, including integer.

use std::io::{self, Cursor, Read, Write};

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_ERROR, STDERR_LAST,
    WorkerClient, WorkerPathInfo,
};

const PATH: &[u8] = b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-output";
const DERIVER: &[u8] = b"/nix/store/11111111111111111111111111111111-input.drv";
const REFERENCE_A: &[u8] = b"/nix/store/22222222222222222222222222222222-a";
const REFERENCE_B: &[u8] = b"/nix/store/33333333333333333333333333333333-b";
const NAR_HASH: &[u8] = b"6c2be2f12a168605ebcba3782286c38eaf3f5d787b7a8bb24a540d2267ff68e1";

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

fn handshake() -> Vec<u8> {
    let mut response = Vec::new();
    integer(&mut response, SERVER_WORKER_MAGIC);
    integer(&mut response, LATEST_WORKER_VERSION.to_wire());
    integer(&mut response, 0);
    byte_string(&mut response, b"2.34.8");
    integer(&mut response, 1);
    integer(&mut response, STDERR_LAST);
    response
}

fn expected_handshake() -> Vec<u8> {
    let mut output = Vec::new();
    integer(&mut output, CLIENT_WORKER_MAGIC);
    integer(&mut output, LATEST_WORKER_VERSION.to_wire());
    integer(&mut output, 0);
    integer(&mut output, 0);
    integer(&mut output, 0);
    output
}

fn connected(response: Vec<u8>) -> WorkerClient<ScriptedStream> {
    let mut input = handshake();
    input.extend(response);
    WorkerClient::connect(ScriptedStream::new(input)).expect("handshake succeeds")
}

#[test]
fn is_valid_path_matches_exact_wire_and_strict_boolean() {
    for (wire, expected) in [(0, false), (1, true)] {
        let mut response = Vec::new();
        integer(&mut response, STDERR_LAST);
        integer(&mut response, wire);
        let mut client = connected(response);

        assert_eq!(client.is_valid_path(PATH).unwrap(), expected);
        assert!(client.profile().capabilities.path_queries);

        let mut exact = expected_handshake();
        integer(&mut exact, 1);
        byte_string(&mut exact, PATH);
        assert_eq!(client.into_inner().output, exact);
    }
}

#[test]
fn query_path_info_decodes_complete_classic_metadata() {
    let mut response = Vec::new();
    integer(&mut response, STDERR_LAST);
    integer(&mut response, 1);
    byte_string(&mut response, DERIVER);
    byte_string(&mut response, NAR_HASH);
    integer(&mut response, 2);
    byte_string(&mut response, REFERENCE_A);
    byte_string(&mut response, REFERENCE_B);
    integer(&mut response, 1234);
    integer(&mut response, 5678);
    integer(&mut response, 1);
    integer(&mut response, 2);
    byte_string(&mut response, b"cache.example-1:signature-a");
    byte_string(&mut response, b"cache.example-1:signature-b");
    byte_string(&mut response, b"fixed:sha256:abc");
    let mut client = connected(response);

    let info = client.query_path_info(PATH).unwrap().unwrap();
    assert_eq!(
        info,
        WorkerPathInfo::new(
            Some(DERIVER.to_vec()),
            String::from_utf8(NAR_HASH.to_vec()).unwrap(),
            vec![REFERENCE_A.to_vec(), REFERENCE_B.to_vec()],
            1234,
            5678,
            true,
            vec![
                b"cache.example-1:signature-a".to_vec(),
                b"cache.example-1:signature-b".to_vec()
            ],
            Some(b"fixed:sha256:abc".to_vec()),
        )
    );
    let mut exact = expected_handshake();
    integer(&mut exact, 26);
    byte_string(&mut exact, PATH);
    assert_eq!(client.into_inner().output, exact);
}

#[test]
fn query_path_info_returns_typed_missing_path() {
    let mut response = Vec::new();
    integer(&mut response, STDERR_LAST);
    integer(&mut response, 0);
    assert_eq!(connected(response).query_path_info(PATH).unwrap(), None);
}

#[test]
fn malformed_request_is_rejected_before_operation_write() {
    for path in [
        b"relative".as_slice(),
        b"/nix/store/short-output".as_slice(),
        b"/nix/store/0123456789abcdeohijklmnpqrsvwxyz-output".as_slice(),
        b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-".as_slice(),
        b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-name/child".as_slice(),
        b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-name\0".as_slice(),
    ] {
        let mut client = connected(Vec::new());
        assert!(client.is_valid_path(path).is_err());
        assert_eq!(client.into_inner().output, expected_handshake());
    }
}

#[test]
fn hostile_metadata_is_rejected() {
    let cases = [
        response_with(|out| integer(out, 2)),
        present_info_with(|out| byte_string(out, b"bad-hash")),
        present_info_with(|out| {
            byte_string(out, NAR_HASH);
            integer(out, 257);
        }),
    ];
    for response in cases {
        let error = connected(response).query_path_info(PATH).unwrap_err();
        assert_eq!(error.to_string(), "Nix daemon operation failed");
    }
}

#[test]
fn duplicate_references_and_signatures_are_rejected() {
    let mut duplicate_reference = Vec::new();
    integer(&mut duplicate_reference, STDERR_LAST);
    integer(&mut duplicate_reference, 1);
    byte_string(&mut duplicate_reference, b"");
    byte_string(&mut duplicate_reference, NAR_HASH);
    integer(&mut duplicate_reference, 2);
    byte_string(&mut duplicate_reference, REFERENCE_A);
    byte_string(&mut duplicate_reference, REFERENCE_A);
    for value in [0, 0, 0, 0] {
        integer(&mut duplicate_reference, value);
    }
    byte_string(&mut duplicate_reference, b"");
    assert!(
        connected(duplicate_reference)
            .query_path_info(PATH)
            .is_err()
    );

    let mut duplicate_signature = Vec::new();
    integer(&mut duplicate_signature, STDERR_LAST);
    integer(&mut duplicate_signature, 1);
    byte_string(&mut duplicate_signature, b"");
    byte_string(&mut duplicate_signature, NAR_HASH);
    integer(&mut duplicate_signature, 0);
    for value in [0, 0, 0] {
        integer(&mut duplicate_signature, value);
    }
    integer(&mut duplicate_signature, 2);
    byte_string(&mut duplicate_signature, b"same");
    byte_string(&mut duplicate_signature, b"same");
    byte_string(&mut duplicate_signature, b"");
    assert!(
        connected(duplicate_signature)
            .query_path_info(PATH)
            .is_err()
    );
}

#[test]
fn daemon_error_is_redacted() {
    let mut response = Vec::new();
    integer(&mut response, STDERR_ERROR);
    byte_string(&mut response, b"sensitive-type");
    integer(&mut response, 1);
    byte_string(&mut response, b"sensitive-name");
    byte_string(&mut response, b"sensitive-path-and-message");
    integer(&mut response, 0);
    integer(&mut response, 0);
    let error = connected(response).is_valid_path(PATH).unwrap_err();
    assert_eq!(error.to_string(), "Nix daemon operation failed");
    assert!(!error.to_string().contains("sensitive"));
}

fn response_with(write: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut response = Vec::new();
    integer(&mut response, STDERR_LAST);
    write(&mut response);
    response
}

fn present_info_with(write_prefix: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    response_with(|out| {
        integer(out, 1);
        byte_string(out, b"");
        write_prefix(out);
    })
}
