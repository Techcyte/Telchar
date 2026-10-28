//! Encodes bounded connection options and requires a terminal daemon response.

use std::io::{self, Cursor, Read, Write};

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_LAST, WorkerClient,
    WorkerOptions,
};

struct Stream {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}

impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.input.read(bytes)
    }
}

impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn integer(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn string(bytes: &mut Vec<u8>, value: &[u8]) {
    integer(bytes, value.len() as u64);
    bytes.extend_from_slice(value);
    bytes.resize(bytes.len() + (8 - value.len() % 8) % 8, 0);
}

fn client(response: &[u8]) -> WorkerClient<Stream> {
    let mut input = Vec::new();
    integer(&mut input, SERVER_WORKER_MAGIC);
    integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    integer(&mut input, 0);
    string(&mut input, b"2.34.8");
    integer(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    input.extend_from_slice(response);
    WorkerClient::connect(Stream {
        input: Cursor::new(input),
        output: Vec::new(),
    })
    .unwrap()
}

#[test]
fn options_encode_all_fields_and_empty_override_exactly() {
    let mut client = client(&STDERR_LAST.to_le_bytes());
    let options = WorkerOptions {
        keep_failed: true,
        keep_going: true,
        fallback: true,
        verbosity: 3,
        max_jobs: 4,
        max_silent_time: 12,
        verbose_build: false,
        build_cores: 8,
        use_substitutes: false,
        overrides: vec![(b"builders".to_vec(), Vec::new())],
    };
    client.set_options(&options).unwrap();
    let mut expected = Vec::new();
    for value in [19, 1, 1, 1, 3, 4, 12, 1, 7, 0, 0, 8, 0, 1] {
        integer(&mut expected, value);
    }
    string(&mut expected, b"builders");
    string(&mut expected, b"");
    let output = client.into_inner().output;
    assert!(output.starts_with(&CLIENT_WORKER_MAGIC.to_le_bytes()));
    assert_eq!(&output[output.len() - expected.len()..], expected);
}

#[test]
fn options_require_a_complete_terminal_response() {
    for response in [
        Vec::new(),
        42_u64.to_le_bytes().to_vec(),
        STDERR_LAST.to_le_bytes()[..4].to_vec(),
    ] {
        let mut client = client(&response);
        assert_eq!(
            client
                .set_options(&WorkerOptions::default())
                .unwrap_err()
                .to_string(),
            "Nix daemon SetOptions failed"
        );
    }
}

#[test]
fn oversized_options_do_not_write_an_operation() {
    for overrides in [
        vec![(b"builders".to_vec(), Vec::new()); 257],
        vec![(Vec::new(), Vec::new())],
        vec![(vec![b'x'; 16_385], Vec::new())],
        vec![(b"builders".to_vec(), vec![b'x'; 16_385])],
    ] {
        let handshake_output = client(&[]).into_inner().output;
        let mut client = client(&[]);
        assert!(
            client
                .set_options(&WorkerOptions {
                    overrides,
                    ..WorkerOptions::default()
                })
                .is_err()
        );
        assert_eq!(client.into_inner().output, handshake_output);
    }
}
