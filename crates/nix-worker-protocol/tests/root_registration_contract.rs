//! Tests root registration contract contracts and failure boundaries, including integer.

use std::io::{self, Cursor, Read, Write};

use nix_worker_protocol::{
    WorkerClient, WorkerVersion, CLIENT_WORKER_MAGIC, SERVER_WORKER_MAGIC, STDERR_ERROR,
    STDERR_LAST,
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

fn successful_daemon_response(operation_count: usize) -> Vec<u8> {
    let mut response = Vec::new();
    integer(&mut response, SERVER_WORKER_MAGIC);
    integer(&mut response, WorkerVersion::new(1, 38).to_wire());
    integer(&mut response, 0);
    byte_string(&mut response, b"2.34.8");
    integer(&mut response, 1);
    integer(&mut response, STDERR_LAST);
    for _ in 0..operation_count {
        integer(&mut response, STDERR_LAST);
        integer(&mut response, 1);
    }
    response
}

#[test]
fn ensure_path_uses_cache_only_worker_operation() {
    let stream = ScriptedStream::new(successful_daemon_response(1));
    let mut client = WorkerClient::connect(stream).expect("client handshake succeeds");

    client
        .ensure_path(b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-cached")
        .expect("substitution succeeds");

    let stream = client.into_inner();
    let mut expected = Vec::new();
    integer(&mut expected, CLIENT_WORKER_MAGIC);
    integer(&mut expected, WorkerVersion::new(1, 38).to_wire());
    integer(&mut expected, 0);
    integer(&mut expected, 0);
    integer(&mut expected, 0);
    integer(&mut expected, 10);
    byte_string(
        &mut expected,
        b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-cached",
    );
    assert_eq!(stream.output, expected);
}

#[test]
fn root_registration_uses_modern_profile_and_preserves_operation_bytes() {
    let stream = ScriptedStream::new(successful_daemon_response(2));
    let mut client = WorkerClient::connect(stream).expect("client handshake succeeds");

    client
        .add_temporary_root(b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-leased")
        .expect("temporary root succeeds");
    client
        .add_indirect_root(b"/var/lib/telchar-gc-roots/lease-request-1")
        .expect("indirect root succeeds");

    let stream = client.into_inner();
    let mut expected = Vec::new();
    integer(&mut expected, CLIENT_WORKER_MAGIC);
    integer(&mut expected, WorkerVersion::new(1, 38).to_wire());
    integer(&mut expected, 0);
    integer(&mut expected, 0);
    integer(&mut expected, 0);
    integer(&mut expected, 11);
    byte_string(
        &mut expected,
        b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-leased",
    );
    integer(&mut expected, 12);
    byte_string(&mut expected, b"/var/lib/telchar-gc-roots/lease-request-1");
    assert_eq!(stream.output, expected);
}

#[test]
fn root_registration_supports_an_explicit_logical_store_directory() {
    let store_directory = b"/tmp/telchar-store";
    let store_path = b"/tmp/telchar-store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-leased";
    let stream = ScriptedStream::new(successful_daemon_response(1));
    let mut client = WorkerClient::connect_with_store_directory(stream, store_directory)
        .expect("client handshake succeeds");

    client
        .add_temporary_root(store_path)
        .expect("temporary root succeeds");

    let stream = client.into_inner();
    let mut expected = Vec::new();
    integer(&mut expected, CLIENT_WORKER_MAGIC);
    integer(&mut expected, WorkerVersion::new(1, 38).to_wire());
    integer(&mut expected, 0);
    integer(&mut expected, 0);
    integer(&mut expected, 0);
    integer(&mut expected, 11);
    byte_string(&mut expected, store_path);
    assert_eq!(stream.output, expected);
}

#[test]
fn daemon_error_is_bounded_and_redacted() {
    let mut response = Vec::new();
    integer(&mut response, SERVER_WORKER_MAGIC);
    integer(&mut response, WorkerVersion::new(1, 38).to_wire());
    integer(&mut response, 0);
    byte_string(&mut response, b"2.34.8");
    integer(&mut response, 1);
    integer(&mut response, STDERR_LAST);
    integer(&mut response, STDERR_ERROR);
    byte_string(&mut response, b"Error");
    integer(&mut response, 1);
    byte_string(&mut response, b"daemon");
    byte_string(&mut response, b"sensitive daemon diagnostic");
    integer(&mut response, 0);
    integer(&mut response, 0);
    let stream = ScriptedStream::new(response);
    let mut client = WorkerClient::connect(stream).expect("client handshake succeeds");

    let error = client
        .add_temporary_root(b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-leased")
        .expect_err("daemon error rejects");

    assert_eq!(error.to_string(), "Nix daemon operation failed");
    assert!(!error.to_string().contains("sensitive"));
}
