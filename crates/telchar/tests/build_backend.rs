use std::io;
use std::time::Duration;

use nix_worker_protocol::{ProtocolSessionLimits, WorkerReader};
use telchar::backend::{BuildBackend, BuildExecution, BuildResult, BuildStatus, OutputTrust};
use telchar::build_request::BuildRequest;
use telchar::deployment::DeploymentConfig;

#[test]
fn backend_contract_forwards_logs_and_returns_terminal_result() {
    let build = admitted_request();
    let execution = BuildExecution::new("request-1", &build, Duration::from_secs(30))
        .expect("execution is valid");
    let mut backend = FixtureBackend;
    let mut logs = Vec::new();

    let result = backend
        .execute_with_logs(
            &execution,
            &mut |chunk| {
                logs.extend_from_slice(chunk);
                Ok(())
            },
            &mut || Ok(false),
        )
        .expect("backend succeeds");

    assert_eq!(logs, b"building\n");
    assert_eq!(result.status(), BuildStatus::Built);
    assert_eq!(result.output_trust(), OutputTrust::TrustedExecutor);
    assert_eq!(result.outputs(), &[(b"out".to_vec(), output_path())]);
}

struct FixtureBackend;

impl BuildBackend for FixtureBackend {
    fn execute_with_logs(
        &mut self,
        execution: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        assert_eq!(execution.request_id(), "request-1");
        assert_eq!(execution.timeout(), Duration::from_secs(30));
        assert_eq!(execution.build().system(), "x86_64-linux");
        assert!(!cancelled()?);
        logs(b"building\n")?;
        BuildResult::new(
            BuildStatus::Built,
            vec![(b"out".to_vec(), output_path())],
            OutputTrust::TrustedExecutor,
        )
    }
}

fn admitted_request() -> BuildRequest {
    let output = output_path();
    let mut wire = Vec::new();
    write_string(
        &mut wire,
        b"/nix/store/00000000000000000000000000000000-backend.drv",
    );
    write_integer(&mut wire, 1);
    write_string(&mut wire, b"out");
    write_string(&mut wire, &output);
    write_string(&mut wire, b"");
    write_string(&mut wire, b"");
    write_integer(&mut wire, 0);
    write_string(&mut wire, b"x86_64-linux");
    write_string(&mut wire, b"/bin/sh");
    write_integer(&mut wire, 2);
    write_string(&mut wire, b"-c");
    write_string(&mut wire, b"printf backend > $out");
    write_integer(&mut wire, 4);
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"backend".as_slice()),
        (b"out".as_slice(), output.as_slice()),
        (b"system".as_slice(), b"x86_64-linux".as_slice()),
    ] {
        write_string(&mut wire, key);
        write_string(&mut wire, value);
    }
    write_integer(&mut wire, 0);
    let mut reader = WorkerReader::new(wire.as_slice(), ProtocolSessionLimits::DEFAULT);
    let worker = reader
        .complete_build_derivation()
        .expect("worker request parses");
    BuildRequest::from_worker_request(
        &worker,
        &DeploymentConfig::parse("x86_64-linux", "").expect("deployment parses"),
    )
    .expect("request admits")
}

fn write_integer(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_string(output: &mut Vec<u8>, value: &[u8]) {
    write_integer(output, value.len() as u64);
    output.extend_from_slice(value);
    output.extend_from_slice(&[0; 7][..(8 - value.len() % 8) % 8]);
}

fn output_path() -> Vec<u8> {
    b"/nix/store/33333333333333333333333333333333-backend".to_vec()
}
