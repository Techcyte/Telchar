//! Tests daemon build derivation contract contracts and failure boundaries, including integer.

use std::io::{self, Cursor, Read, Write};

use nix_worker_protocol::{
    BuildDerivationClientRequest, BuildDerivationOutputRequest, CLIENT_WORKER_MAGIC,
    LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_ERROR, STDERR_LAST, STDERR_NEXT,
    WorkerBuildStatus, WorkerClient,
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
fn accepts_successful_daemon_build_statuses() {
    for status in [0, 1, 2, 13] {
        let mut input = Vec::new();
        handshake(&mut input, 1);
        integer(&mut input, STDERR_LAST);
        integer(&mut input, status);
        string(&mut input, b"");
        for value in [1, 0, 10, 20, 0, 0, 0] {
            integer(&mut input, value);
        }
        let outputs = [BuildDerivationOutputRequest {
            name: b"out",
            path: OUTPUT,
            hash_algorithm: b"",
            hash: b"",
        }];
        let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();
        let result = client.build_derivation(&request(&outputs), &mut |_| Ok(()));
        let result = result.unwrap_or_else(|error| panic!("successful status {status}: {error}"));
        assert_eq!(
            result.status(),
            if status == 0 {
                WorkerBuildStatus::Built
            } else {
                WorkerBuildStatus::AlreadyValid
            }
        );
    }
}

#[test]
fn failed_build_consumes_result_before_next_operation() {
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 5);
    string(&mut input, b"hash mismatch");
    for value in [1, 0, 10, 20, 0, 0, 0] {
        integer(&mut input, value);
    }
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 1);
    let outputs = [BuildDerivationOutputRequest {
        name: b"out",
        path: OUTPUT,
        hash_algorithm: b"",
        hash: b"",
    }];
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();
    let result = client
        .build_derivation(&request(&outputs), &mut |_| Ok(()))
        .expect("ordinary Nix failure is a complete build result");
    assert!(result.outputs().is_empty());
    assert_eq!(result.status(), WorkerBuildStatus::Failed(5));
    assert_eq!(result.message(), "hash mismatch");
    assert!(client.is_valid_path(OUTPUT).unwrap());
}

#[test]
fn failed_build_rejects_incomplete_or_invalid_result_fields() {
    for trailer in [
        vec![],
        1_u64.to_le_bytes().to_vec(),
        [1_u64, 2].into_iter().flat_map(u64::to_le_bytes).collect(),
    ] {
        let mut input = Vec::new();
        handshake(&mut input, 1);
        integer(&mut input, STDERR_LAST);
        integer(&mut input, 5);
        string(&mut input, b"hash mismatch secret");
        input.extend(trailer);
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
            "Nix daemon BuildDerivation result failed"
        );
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
fn builds_exact_local_derivation_targets_with_results() {
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 1); // one keyed result
    string(&mut input, DRV);
    integer(&mut input, 0); // Built
    string(&mut input, b"");
    integer(&mut input, 1);
    integer(&mut input, 0);
    integer(&mut input, 10);
    integer(&mut input, 20);
    integer(&mut input, 0);
    integer(&mut input, 0);
    integer(&mut input, 0); // no built outputs
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

    client
        .build_paths_with_results(&[DRV.to_vec()])
        .expect("local derivation target builds");

    let wire = client.into_inner().output;
    let mut operation = Vec::new();
    integer(&mut operation, 46);
    integer(&mut operation, 1);
    string(&mut operation, DRV);
    integer(&mut operation, 0);
    assert!(wire.ends_with(&operation));
}

#[test]
fn builds_exact_selected_output_target_with_results() {
    const TARGET: &[u8] = b"/nix/store/00000000000000000000000000000000-contract.drv!out";
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 1);
    string(&mut input, TARGET);
    integer(&mut input, 0);
    string(&mut input, b"");
    integer(&mut input, 1);
    integer(&mut input, 0);
    integer(&mut input, 10);
    integer(&mut input, 20);
    integer(&mut input, 0);
    integer(&mut input, 0);
    integer(&mut input, 0);
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

    client
        .build_paths_with_results(&[TARGET.to_vec()])
        .expect("selected output target builds");

    let wire = client.into_inner().output;
    let mut operation = Vec::new();
    integer(&mut operation, 46);
    integer(&mut operation, 1);
    string(&mut operation, TARGET);
    integer(&mut operation, 0);
    assert!(wire.ends_with(&operation));
}

#[test]
fn rejects_malformed_selected_output_targets_before_writing_operation() {
    for target in [
        b"/nix/store/00000000000000000000000000000000-contract.drv!".as_slice(),
        b"/nix/store/00000000000000000000000000000000-contract.drv!out!dev".as_slice(),
        b"/nix/store/00000000000000000000000000000000-contract.drv!out/name".as_slice(),
        b"/nix/store/00000000000000000000000000000000-contract!out".as_slice(),
    ] {
        let mut input = Vec::new();
        handshake(&mut input, 1);
        let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

        client
            .build_paths_with_results(&[target.to_vec()])
            .expect_err("malformed selected output target is rejected");

        assert_eq!(client.into_inner().output.len(), 40);
    }
}

#[test]
fn preserves_dependency_terminal_status_and_category_without_peer_payload() {
    const TARGET: &[u8] = b"/nix/store/00000000000000000000000000000000-contract.drv!out";
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 1);
    string(&mut input, TARGET);
    integer(&mut input, 4);
    string(
        &mut input,
        b"interrupted by the user secret=/run/credentials/token",
    );
    for value in [1, 0, 10, 20, 0, 0, 0] {
        integer(&mut input, value);
    }
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

    let error = client
        .build_paths_with_results(&[TARGET.to_vec()])
        .expect_err("terminal dependency failure is rejected");

    assert_eq!(
        nix_worker_protocol::build_paths_failure_result(&error),
        Some(nix_worker_protocol::BuildPathsFailureResult {
            target: TARGET.to_vec(),
            status: 4,
            category: "interrupted",
        })
    );
    assert!(!error.to_string().contains("credentials"));
    assert!(!error.to_string().contains("token"));
}

#[test]
fn classifies_missing_result_count_after_terminal_frame() {
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

    let error = client
        .build_paths_with_results(&[DRV.to_vec()])
        .expect_err("missing result count fails");

    assert_eq!(
        nix_worker_protocol::build_paths_failure_phase(&error),
        Some(nix_worker_protocol::BuildPathsFailurePhase::Count)
    );
}

#[test]
fn classifies_selected_output_target_mismatch_without_peer_payload() {
    const TARGET: &[u8] = b"/nix/store/00000000000000000000000000000000-contract.drv!out";
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 1);
    string(&mut input, DRV);
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();

    let error = client
        .build_paths_with_results(&[TARGET.to_vec()])
        .expect_err("mismatched keyed target fails");

    assert_eq!(
        nix_worker_protocol::build_paths_failure_phase(&error),
        Some(nix_worker_protocol::BuildPathsFailurePhase::Target)
    );
    assert!(
        !error
            .to_string()
            .contains(String::from_utf8_lossy(DRV).as_ref())
    );
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
    assert!(
        wire.windows(authority.len())
            .any(|window| window == authority)
    );
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
fn connection_without_trust_rejects_input_addressed_build_before_operation_bytes() {
    for trust in [0, 2] {
        let mut input = Vec::new();
        handshake(&mut input, trust);
        let outputs = [BuildDerivationOutputRequest {
            name: b"out",
            path: OUTPUT,
            hash_algorithm: b"",
            hash: b"",
        }];
        let client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();
        let handshake_bytes = client.into_inner().output;

        let mut input = Vec::new();
        handshake(&mut input, trust);
        let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();
        let error = client
            .build_derivation(&request(&outputs), &mut |_| Ok(()))
            .unwrap_err();

        assert_eq!(error.to_string(), "Nix daemon operation failed");
        assert_eq!(client.into_inner().output, handshake_bytes);
    }
}

#[test]
fn response_failure_identifies_build_derivation_phase() {
    let mut input = Vec::new();
    handshake(&mut input, 1);
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
        "Nix daemon BuildDerivation response failed: failed to fill whole buffer"
    );
}

#[test]
fn daemon_rejection_preserves_bounded_build_diagnostic() {
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_ERROR);
    string(&mut input, b"Error");
    integer(&mut input, 1);
    string(&mut input, b"daemon");
    string(&mut input, b"builder failed while compiling telchar");
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

    assert_eq!(
        error.to_string(),
        "Nix daemon BuildDerivation was rejected: builder failed while compiling telchar"
    );
}

#[test]
fn terminal_build_failure_preserves_bounded_message_and_status() {
    for status in [3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14] {
        let message = "builder failed with exit code 3";
        let mut input = Vec::new();
        handshake(&mut input, 1);
        integer(&mut input, STDERR_LAST);
        integer(&mut input, status);
        string(&mut input, message.as_bytes());
        for value in [1, 0, 10, 20, 0, 0, 0] {
            integer(&mut input, value);
        }
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
        assert_eq!(result.status(), WorkerBuildStatus::Failed(status));
        assert_eq!(result.message(), message);
    }
}

#[test]
fn terminal_build_message_is_truncated_at_a_utf8_boundary() {
    // Lossy decoding can expand a bounded non-UTF-8 peer string.
    let bytes = vec![0xff; nix_worker_protocol::MAXIMUM_BUILD_FAILURE_MESSAGE_BYTES];
    let message = String::from_utf8_lossy(&bytes);
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 6);
    string(&mut input, &bytes);
    for value in [1, 0, 10, 20, 0, 0, 0] {
        integer(&mut input, value);
    }
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
    assert_eq!(result.status(), WorkerBuildStatus::Failed(6));
    assert!(result.message().len() <= nix_worker_protocol::MAXIMUM_BUILD_FAILURE_MESSAGE_BYTES);
    assert!(result.message().len() >= nix_worker_protocol::MAXIMUM_BUILD_FAILURE_MESSAGE_BYTES - 2);
    assert!(message.starts_with(result.message()));
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

#[test]
fn failed_keyed_build_consumes_all_results_before_next_operation() {
    let second = b"/nix/store/11111111111111111111111111111111-second.drv";
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 2);
    for (target, status, message) in [
        (DRV, 4, b"builder failed with exit code 3".as_slice()),
        (second.as_slice(), 0, b"".as_slice()),
    ] {
        string(&mut input, target);
        integer(&mut input, status);
        string(&mut input, message);
        for _ in 0..7 {
            integer(&mut input, 0);
        }
    }
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 1);
    let mut client = WorkerClient::connect(ScriptedStream::new(input)).unwrap();
    let error = client
        .build_paths_with_results(&[DRV.to_vec(), second.to_vec()])
        .unwrap_err();
    assert_eq!(
        nix_worker_protocol::build_paths_failure_result(&error)
            .unwrap()
            .status,
        4
    );
    assert!(client.is_valid_path(OUTPUT).unwrap());
}

#[test]
fn terminal_diagnostic_omits_nix_embedded_log_excerpt() {
    let mut input = Vec::new();
    handshake(&mut input, 1);
    integer(&mut input, STDERR_LAST);
    integer(&mut input, 3);
    string(&mut input, b"Cannot build derivation.\nReason: builder failed with exit code 3.\nLast 25 log lines:\n> private-log-marker\nFor full logs, run nix-store -l");
    for _ in 0..7 {
        integer(&mut input, 0);
    }
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
    assert_eq!(
        result.message(),
        "Cannot build derivation.\nReason: builder failed with exit code 3.\n"
    );
    assert_eq!(result.status(), WorkerBuildStatus::Failed(3));
}
