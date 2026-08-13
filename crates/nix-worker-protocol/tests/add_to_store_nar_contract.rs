//! Tests add to store nar contract contracts and failure boundaries, including integer.

use std::io::{self, Cursor, Read, Write};

use nix_worker_protocol::{
    AddToStoreNarInfo, CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC,
    STDERR_LAST, WorkerClient, WorkerVersion,
};

const PATH: &[u8] = b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-output";
const REFERENCE: &[u8] = b"/nix/store/11111111111111111111111111111111-reference";
const NAR_HASH: &str = "6c2be2f12a168605ebcba3782286c38eaf3f5d787b7a8bb24a540d2267ff68e1";

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

fn handshake(trust: u64) -> Vec<u8> {
    let mut response = Vec::new();
    integer(&mut response, SERVER_WORKER_MAGIC);
    integer(&mut response, LATEST_WORKER_VERSION.to_wire());
    integer(&mut response, 0); // daemon feature set
    byte_string(&mut response, b"2.34.8");
    integer(&mut response, trust);
    integer(&mut response, STDERR_LAST);
    response
}

fn expected_handshake() -> Vec<u8> {
    let mut output = Vec::new();
    integer(&mut output, CLIENT_WORKER_MAGIC);
    integer(&mut output, LATEST_WORKER_VERSION.to_wire());
    integer(&mut output, 0); // client feature set
    integer(&mut output, 0); // obsolete CPU affinity field
    integer(&mut output, 0); // obsolete reserve-space field
    output
}

fn info(references: &[Vec<u8>]) -> AddToStoreNarInfo<'_> {
    AddToStoreNarInfo {
        path: PATH,
        deriver: None,
        nar_hash_hex: NAR_HASH,
        references,
        registration_time: 0,
        nar_size: 12,
        ultimate: false,
        signatures: &[],
        content_address: None,
    }
}

#[test]
fn writes_exact_add_to_store_nar_metadata_and_framed_body() {
    let mut response = handshake(1);
    response.extend_from_slice(&STDERR_LAST.to_le_bytes());
    let mut client = WorkerClient::connect(ScriptedStream::new(response)).unwrap();
    let references = [REFERENCE.to_vec()];

    client
        .add_to_store_nar(
            &info(&references),
            &mut b"streamed-nar".as_slice(),
            false,
            true,
        )
        .unwrap();

    let mut expected = expected_handshake();
    integer(&mut expected, 39); // WorkerProto::Op::AddToStoreNar
    byte_string(&mut expected, PATH);
    byte_string(&mut expected, b""); // no deriver
    byte_string(&mut expected, NAR_HASH.as_bytes());
    integer(&mut expected, 1); // one reference
    byte_string(&mut expected, REFERENCE);
    integer(&mut expected, 0); // registration time
    integer(&mut expected, 12); // declared NAR size
    integer(&mut expected, 0); // not ultimate
    integer(&mut expected, 0); // no signatures
    byte_string(&mut expected, b""); // no content address
    integer(&mut expected, 0); // repair disabled
    integer(&mut expected, 1); // signature checking disabled on trusted connection
    integer(&mut expected, 12); // one framed NAR chunk
    expected.extend_from_slice(b"streamed-nar");
    integer(&mut expected, 0); // framed NAR terminator
    assert_eq!(client.into_inner().output, expected);
}

#[test]
fn untrusted_connection_cannot_disable_signature_checking_before_write() {
    let client = WorkerClient::connect(ScriptedStream::new(handshake(2))).unwrap();
    let initial = client.into_inner().output;
    let mut client = WorkerClient::connect(ScriptedStream::new(handshake(2))).unwrap();
    let references = [REFERENCE.to_vec()];

    let error = client
        .add_to_store_nar(
            &info(&references),
            &mut b"streamed-nar".as_slice(),
            false,
            true,
        )
        .unwrap_err();

    assert_eq!(error.to_string(), "Nix daemon operation failed");
    assert_eq!(client.into_inner().output, initial);
}

#[test]
fn malformed_metadata_is_rejected_before_operation_write() {
    let references = [REFERENCE.to_vec()];
    let duplicate_references = [PATH.to_vec(), PATH.to_vec()];
    let malformed = [
        AddToStoreNarInfo {
            path: b"relative",
            ..info(&references)
        },
        AddToStoreNarInfo {
            nar_hash_hex: "bad",
            ..info(&references)
        },
        AddToStoreNarInfo {
            references: &duplicate_references,
            ..info(&references)
        },
        AddToStoreNarInfo {
            ultimate: true,
            ..info(&references)
        },
    ];
    for info in malformed {
        let mut client = WorkerClient::connect(ScriptedStream::new(handshake(1))).unwrap();
        let error = client
            .add_to_store_nar(&info, &mut b"body".as_slice(), false, true)
            .unwrap_err();
        assert_eq!(error.to_string(), "Nix daemon operation failed");
        assert_eq!(client.into_inner().output, expected_handshake());
    }
}

#[test]
fn source_failure_stops_without_successfully_terminating_frame() {
    struct FailingSource(bool);
    impl Read for FailingSource {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.0 {
                return Err(io::Error::other("sensitive source failure"));
            }
            self.0 = true;
            output[..3].copy_from_slice(b"nar");
            Ok(3)
        }
    }
    let mut client = WorkerClient::connect(ScriptedStream::new(handshake(1))).unwrap();

    let references = [REFERENCE.to_vec()];
    let error = client
        .add_to_store_nar(&info(&references), &mut FailingSource(false), false, true)
        .unwrap_err();

    assert_eq!(error.to_string(), "Nix daemon operation failed");
    let output = client.into_inner().output;
    assert!(output.ends_with(b"nar"));
    assert!(!output.ends_with(&0_u64.to_le_bytes()));
}

#[test]
fn daemon_error_is_redacted_after_upload() {
    let mut response = handshake(1);
    response.extend_from_slice(&nix_worker_protocol::STDERR_ERROR.to_le_bytes());
    byte_string(&mut response, b"sensitive-type");
    integer(&mut response, 1);
    byte_string(&mut response, b"sensitive-name");
    byte_string(&mut response, b"sensitive-path-and-message");
    integer(&mut response, 0);
    integer(&mut response, 0);
    let mut client = WorkerClient::connect(ScriptedStream::new(response)).unwrap();
    let references = [REFERENCE.to_vec()];

    let error = client
        .add_to_store_nar(
            &info(&references),
            &mut b"streamed-nar".as_slice(),
            false,
            true,
        )
        .unwrap_err();

    assert_eq!(error.to_string(), "Nix daemon operation failed");
    assert!(!error.to_string().contains("sensitive"));
}

#[test]
fn operation_requires_supported_framed_nar_profile() {
    let mut response = Vec::new();
    integer(&mut response, SERVER_WORKER_MAGIC);
    integer(&mut response, WorkerVersion::new(1, 35).to_wire());
    byte_string(&mut response, b"2.18");
    integer(&mut response, 1);
    integer(&mut response, STDERR_LAST); // handshake terminal frame
    integer(&mut response, STDERR_LAST); // AddToStoreNar terminal frame
    let mut client = WorkerClient::connect(ScriptedStream::new(response)).unwrap();
    let references = [REFERENCE.to_vec()];

    client
        .add_to_store_nar(
            &info(&references),
            &mut b"streamed-nar".as_slice(),
            false,
            true,
        )
        .expect("all supported profiles have framed AddToStoreNar");
}
