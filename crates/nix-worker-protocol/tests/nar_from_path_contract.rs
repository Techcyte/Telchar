use std::io::{self, Cursor, Read, Write};

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_ERROR, STDERR_LAST,
    WorkerClient,
};

const PATH: &[u8] = b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-output";

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

fn response(body: &[u8]) -> Vec<u8> {
    let mut input = Vec::new();
    integer(&mut input, SERVER_WORKER_MAGIC);
    integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    integer(&mut input, 0); // daemon features
    byte_string(&mut input, b"2.34.8");
    integer(&mut input, 1); // trusted connection
    integer(&mut input, STDERR_LAST); // handshake terminal frame
    integer(&mut input, STDERR_LAST); // NarFromPath operation terminal frame
    input.extend_from_slice(body); // raw, unframed NAR bytes until daemon EOF
    input
}

fn expected_request() -> Vec<u8> {
    let mut output = Vec::new();
    integer(&mut output, CLIENT_WORKER_MAGIC);
    integer(&mut output, LATEST_WORKER_VERSION.to_wire());
    integer(&mut output, 0); // client features
    integer(&mut output, 0); // obsolete CPU affinity field
    integer(&mut output, 0); // obsolete reserve-space field
    integer(&mut output, 38); // WorkerProto::Op::NarFromPath
    byte_string(&mut output, PATH);
    output
}

#[test]
fn writes_exact_request_and_streams_raw_body_until_eof() {
    let mut client = WorkerClient::connect(ScriptedStream::new(response(b"raw-nar"))).unwrap();
    let mut output = Vec::new();

    client.nar_from_path(PATH, 7, &mut output).unwrap();

    assert_eq!(output, b"raw-nar");
    assert_eq!(client.into_inner().output, expected_request());
}

#[test]
fn malformed_path_is_rejected_before_operation_write() {
    let mut client = WorkerClient::connect(ScriptedStream::new(response(b"unused"))).unwrap();
    let mut output = Vec::new();

    let error = client
        .nar_from_path(b"relative", 6, &mut output)
        .unwrap_err();

    assert_eq!(error.to_string(), "Nix daemon operation failed");
    let mut handshake = expected_request();
    handshake.truncate(handshake.len() - 8 - 8 - PATH.len().div_ceil(8) * 8);
    assert_eq!(client.into_inner().output, handshake);
    assert!(output.is_empty());
}

#[test]
fn daemon_error_is_redacted_before_raw_body() {
    let mut input = response(b"");
    let handshake_length = input.len() - 8;
    input.truncate(handshake_length);
    integer(&mut input, STDERR_ERROR);
    byte_string(&mut input, b"sensitive-type");
    integer(&mut input, 1);
    byte_string(&mut input, b"sensitive-name");
    byte_string(&mut input, b"sensitive-path-and-message");
    integer(&mut input, 0);
    integer(&mut input, 0);
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

    let error = client.nar_from_path(PATH, 0, &mut Vec::new()).unwrap_err();

    assert_eq!(error.to_string(), "Nix daemon operation failed");
    assert!(!error.to_string().contains("sensitive"));
}

#[test]
fn writer_failure_stops_streaming_and_is_redacted() {
    struct FailingWriter;
    impl Write for FailingWriter {
        fn write(&mut self, _input: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "sensitive sink"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut client = WorkerClient::connect(ScriptedStream::new(response(b"raw-nar"))).unwrap();

    let error = client
        .nar_from_path(PATH, 7, &mut FailingWriter)
        .unwrap_err();

    assert_eq!(error.to_string(), "Nix daemon operation failed");
    assert!(!error.to_string().contains("sensitive"));
}
