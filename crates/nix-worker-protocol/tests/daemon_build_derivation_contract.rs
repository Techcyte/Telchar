//! Tests daemon build derivation contract contracts and failure boundaries, including integer.

use std::io::{self, Cursor, Read, Write};

use nix_worker_protocol::{
    BuildDerivationClientRequest, BuildDerivationOutputRequest, WorkerBuildStatus, WorkerClient,
    CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_ERROR, STDERR_LAST,
    STDERR_NEXT,
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
        hash_algorithm: b"",
        hash: b"",
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
fn writes_exact_fixed_output_authority() {
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 2); // AlreadyValid
    string(&mut input, b"");
    integer(&mut input, 0);
    integer(&mut input, 1);
    integer(&mut input, 0);
    integer(&mut input, 0);
    integer(&mut input, 0);
    integer(&mut input, 0);
    integer(&mut input, 0); // no built outputs
    let hash = b"0000000000000000000000000000000000000000000000000000000000000000";
    let outputs = [BuildDerivationOutputRequest {
        name: b"out",
        path: OUTPUT,
        hash_algorithm: b"r:sha256",
        hash,
    }];
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

    client
        .build_derivation(&request(&outputs), &mut |_| Ok(()))
        .unwrap();

    let wire = client.into_inner().output;
    let mut authority = Vec::new();
    string(&mut authority, b"out");
    string(&mut authority, OUTPUT);
    string(&mut authority, b"r:sha256");
    string(&mut authority, hash);
    assert!(wire
        .windows(authority.len())
        .any(|window| window == authority));
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
        hash_algorithm: b"",
        hash: b"",
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
        hash_algorithm: b"",
        hash: b"",
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
fn daemon_rejection_is_redacted_and_identifies_build_phase() {
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_ERROR);
    string(&mut input, b"sensitive-type");
    integer(&mut input, 1);
    string(&mut input, b"sensitive-name");
    string(&mut input, b"sensitive-message");
    integer(&mut input, 0);
    integer(&mut input, 0);
    let outputs = [BuildDerivationOutputRequest {
        name: b"out",
        path: OUTPUT,
        hash_algorithm: b"",
        hash: b"",
    }];
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

    let error = client
        .build_derivation(&request(&outputs), &mut |_| Ok(()))
        .unwrap_err();

    assert_eq!(error.to_string(), "Nix daemon BuildDerivation was rejected");
    assert!(!error.to_string().contains("sensitive"));
}

#[test]
fn terminal_build_failure_exposes_only_allowlisted_reason() {
    for (message, expected) in [
        (
            "builder for '/nix/store/secret-build.drv' failed with exit code 1",
            "Nix daemon BuildDerivation failed with status 3 category builder-exited",
        ),
        (
            "cannot build derivation because required input is missing",
            "Nix daemon BuildDerivation failed with status 3 category missing-input",
        ),
        (
            "sensitive arbitrary daemon details",
            "Nix daemon BuildDerivation failed with status 3 category unknown",
        ),
    ] {
        let mut input = Vec::new();
        handshake(&mut input, 1);
        integer(&mut input, STDERR_LAST);
        integer(&mut input, 3); // PermanentFailure
        string(&mut input, message.as_bytes());
        let outputs = [BuildDerivationOutputRequest {
            name: b"out",
            path: OUTPUT,
            hash_algorithm: b"",
            hash: b"",
        }];
        let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

        let error = client
            .build_derivation(&request(&outputs), &mut |_| Ok(()))
            .unwrap_err();

        assert_eq!(error.to_string(), expected);
        assert!(!error.to_string().contains("secret"));
        assert!(!error.to_string().contains("sensitive"));
    }
}

#[test]
fn terminal_build_status_is_preserved_without_daemon_message() {
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 6); // TimedOut
    string(&mut input, b"sensitive daemon message");
    let outputs = [BuildDerivationOutputRequest {
        name: b"out",
        path: OUTPUT,
        hash_algorithm: b"",
        hash: b"",
    }];
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

    let error = client
        .build_derivation(&request(&outputs), &mut |_| Ok(()))
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "Nix daemon BuildDerivation failed with status 6 category unknown"
    );
    assert!(!error.to_string().contains("sensitive"));
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
            hash_algorithm: b"",
            hash: b"",
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

        assert_eq!(
            error.to_string(),
            if fail_logs {
                "Nix daemon BuildDerivation log sink failed"
            } else {
                "Nix daemon BuildDerivation result failed"
            }
        );
        assert!(!error.to_string().contains("sensitive"));
    }
}
