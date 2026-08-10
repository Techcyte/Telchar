use std::io::{self, Cursor, Read, Write};

use nix_worker_protocol::{
    BuildDerivationClientRequest, BuildDerivationOutputRequest, WorkerBuildStatus, WorkerClient,
    CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_LAST, STDERR_NEXT,
};

const DRV: &[u8] = b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-example.drv";
const OUTPUT: &[u8] = b"/nix/store/11111111111111111111111111111111-example";

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

fn string(output: &mut Vec<u8>, value: &[u8]) {
    integer(output, value.len() as u64);
    output.extend_from_slice(value);
    output.resize(output.len() + (8 - value.len() % 8) % 8, 0);
}

fn handshake(input: &mut Vec<u8>, trust: u64) {
    integer(input, SERVER_WORKER_MAGIC);
    integer(input, LATEST_WORKER_VERSION.to_wire());
    integer(input, 0); // daemon features
    string(input, b"2.34.8");
    integer(input, trust);
    integer(input, STDERR_LAST); // handshake terminal frame
}

fn request<'a>(
    outputs: &'a [BuildDerivationOutputRequest<'a>],
) -> BuildDerivationClientRequest<'a> {
    BuildDerivationClientRequest {
        drv_path: DRV,
        outputs,
        input_sources: &[],
        platform: b"x86_64-linux",
        builder: b"/bin/sh",
        arguments: &[],
        environment: &[],
    }
}

#[test]
fn writes_exact_input_addressed_request_streams_logs_and_reads_built_outputs() {
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_NEXT);
    string(&mut input, b"build log\n");
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 0); // Built
    string(&mut input, b""); // no error
    integer(&mut input, 1); // times built
    integer(&mut input, 0); // deterministic
    integer(&mut input, 10); // start
    integer(&mut input, 20); // stop
    integer(&mut input, 0); // no user CPU duration
    integer(&mut input, 0); // no system CPU duration
    integer(&mut input, 1); // one built output
    string(
        &mut input,
        b"sha256:0000000000000000000000000000000000000000000000000000000000000000!out",
    );
    string(
        &mut input,
        format!(r#"{{"outPath":"{}"}}"#, String::from_utf8_lossy(OUTPUT)).as_bytes(),
    );
    let outputs = [BuildDerivationOutputRequest {
        name: b"out",
        path: OUTPUT,
    }];
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();
    let mut logs = Vec::new();

    let result = client
        .build_derivation(&request(&outputs), &mut |chunk| {
            logs.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();

    assert_eq!(result.status(), WorkerBuildStatus::Built);
    assert_eq!(result.outputs(), &[(b"out".to_vec(), OUTPUT.to_vec())]);
    assert_eq!(logs, b"build log\n");
    let wire = client.into_inner().output;
    let mut operation = Vec::new();
    integer(&mut operation, 36); // WorkerProto::Op::BuildDerivation
    string(&mut operation, DRV);
    integer(&mut operation, 1);
    string(&mut operation, b"out");
    string(&mut operation, OUTPUT);
    string(&mut operation, b""); // input-addressed hash algorithm
    string(&mut operation, b""); // input-addressed hash
    integer(&mut operation, 0); // no input sources
    string(&mut operation, b"x86_64-linux");
    string(&mut operation, b"/bin/sh");
    integer(&mut operation, 0); // no arguments
    integer(&mut operation, 0); // no environment
    integer(&mut operation, 0); // normal build mode
    assert!(wire.ends_with(&operation));
    assert!(wire.starts_with(&CLIENT_WORKER_MAGIC.to_le_bytes()));
}

#[test]
fn accepts_store_relative_realisation_output_paths() {
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 0); // Built
    string(&mut input, b""); // no error
    integer(&mut input, 1); // times built
    integer(&mut input, 0); // deterministic
    integer(&mut input, 10); // start
    integer(&mut input, 20); // stop
    integer(&mut input, 0); // no user CPU duration
    integer(&mut input, 0); // no system CPU duration
    integer(&mut input, 1); // one built output
    string(
        &mut input,
        b"sha256:0000000000000000000000000000000000000000000000000000000000000000!out",
    );
    string(
        &mut input,
        br#"{"outPath":"11111111111111111111111111111111-example"}"#,
    );
    let outputs = [BuildDerivationOutputRequest {
        name: b"out",
        path: OUTPUT,
    }];
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

    let result = client
        .build_derivation(&request(&outputs), &mut |_| Ok(()))
        .unwrap();

    assert_eq!(result.outputs(), &[(b"out".to_vec(), OUTPUT.to_vec())]);
}

#[test]
fn untrusted_connection_rejects_input_addressed_build_before_operation_bytes() {
    let mut input = Vec::new();
    handshake(&mut input, 2);
    let outputs = [BuildDerivationOutputRequest {
        name: b"out",
        path: OUTPUT,
    }];
    let client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();
    let handshake_bytes = client.into_inner().output;

    let mut input = Vec::new();
    handshake(&mut input, 2);
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();
    let error = client
        .build_derivation(&request(&outputs), &mut |_| Ok(()))
        .unwrap_err();

    assert_eq!(error.to_string(), "Nix daemon operation failed");
    assert_eq!(client.into_inner().output, handshake_bytes);
}

#[test]
fn malformed_result_and_log_writer_failure_are_redacted() {
    for (label, status, fail_logs) in [("status", 99, false), ("logs", 0, true)] {
        let mut input = Vec::new();
        handshake(&mut input, 1);
        integer(&mut input, STDERR_NEXT);
        string(&mut input, b"sensitive daemon log");
        integer(&mut input, STDERR_LAST);
        integer(&mut input, status);
        let outputs = [BuildDerivationOutputRequest {
            name: b"out",
            path: OUTPUT,
        }];
        let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

        let error = client
            .build_derivation(&request(&outputs), &mut |_| {
                if fail_logs {
                    Err(io::Error::other("sensitive writer"))
                } else {
                    Ok(())
                }
            })
            .expect_err(label);

        assert_eq!(error.to_string(), "Nix daemon operation failed");
        assert!(!error.to_string().contains("sensitive"));
    }
}
