use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use nix_worker_protocol::{ProtocolSessionLimits, WorkerReader};
use telchar::build_request::BuildRequest;
use telchar::deployment::DeploymentConfig;
use telchar::local_executor::{
    BuildExecutor, GatewayStoreExecutor, LocalBuildStatus, LocalExecutionRequest, NixStoreExecutor,
    OutputTrust,
};
use telchar::nix_fixture::NixFixture;
use telchar::store_daemon::GatewayStoreEndpoint;

const DERIVATION_PATH: &[u8] =
    b"/nix/store/00000000000000000000000000000000-telchar-local-executor.drv";
const OUTPUT_PATH: &[u8] = b"/nix/store/11111111111111111111111111111111-telchar-local-executor";

#[test]
fn local_execution_request_retains_only_the_admitted_build_and_control_metadata() {
    let build = admitted_request();
    let request = LocalExecutionRequest::new("request-1", &build, Duration::from_secs(30))
        .expect("execution request is valid");

    assert_eq!(request.request_id(), "request-1");
    assert_eq!(request.build(), &build);
    assert_eq!(request.timeout(), Duration::from_secs(30));
    assert!(LocalExecutionRequest::new("", &build, Duration::from_secs(30)).is_err());
    assert!(LocalExecutionRequest::new("request-1", &build, Duration::ZERO).is_err());
}

#[test]
fn executor_uses_fixed_helper_and_store_endpoint_without_shell_interpolation() {
    let root = unique_root("arguments");
    fs::create_dir_all(&root).expect("fixture root creates");
    let helper = root.join("record-helper");
    let record = root.join("record.json");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$#\" > '{}'\nprintf '%s\\n' \"$1\" >> '{}'\ncat >> '{}'\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"{}\"]]}}\\n'\n",
            record.display(),
            record.display(),
            record.display(),
            String::from_utf8_lossy(OUTPUT_PATH),
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper is executable");

    let build = admitted_request();
    let request = LocalExecutionRequest::new(
        "request-$(touch should-not-exist)",
        &build,
        Duration::from_secs(5),
    )
    .expect("execution request is valid");
    let mut executor = NixStoreExecutor::new(&helper, "unix:///fixed-gateway.sock")
        .expect("executor config is valid");
    let result = executor.execute(&request).expect("fake helper succeeds");

    assert_eq!(result.status(), LocalBuildStatus::Built);
    assert_eq!(result.outputs(), &[(b"out".to_vec(), OUTPUT_PATH.to_vec())]);
    let recorded = fs::read_to_string(&record).expect("record reads");
    let mut lines = recorded.lines();
    assert_eq!(
        lines.next(),
        Some("1"),
        "helper receives one fixed argument"
    );
    assert_eq!(lines.next(), Some("unix:///fixed-gateway.sock"));
    let body: serde_json::Value =
        serde_json::from_str(&lines.collect::<Vec<_>>().join("\n")).expect("request is JSON");
    assert_eq!(body["version"], 1);
    assert_eq!(body["request_id"], "request-$(touch should-not-exist)");
    assert_eq!(
        body["derivation_path"].as_str(),
        Some(std::str::from_utf8(DERIVATION_PATH).expect("derivation path is UTF-8"))
    );
    assert_eq!(body["system"], "x86_64-linux");
    assert!(!root.join("should-not-exist").exists());
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn built_and_already_valid_results_have_output_trust() {
    for (status, expected_status) in [
        ("built", LocalBuildStatus::Built),
        ("already-valid", LocalBuildStatus::AlreadyValid),
    ] {
        let root = unique_root(status);
        fs::create_dir_all(&root).expect("fixture root creates");
        let helper = root.join("build-helper");
        fs::write(
            &helper,
            format!(
                "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '{{\"version\":1,\"success\":true,\"status\":\"{status}\",\"outputs\":[[\"out\",\"{}\"]]}}\\n'\n",
                String::from_utf8_lossy(OUTPUT_PATH),
            ),
        )
        .expect("helper writes");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))
            .expect("helper is executable");
        let build = admitted_request();
        let request = LocalExecutionRequest::new(status, &build, Duration::from_secs(5))
            .expect("execution request is valid");
        let mut executor = NixStoreExecutor::new(&helper, "unix:///fixed-gateway.sock")
            .expect("executor config is valid");

        let result = executor.execute(&request).expect("helper response parses");

        assert_eq!(result.status(), expected_status);
        assert_eq!(result.output_trust(), OutputTrust::TrustedExecutor);
        fs::remove_dir_all(root).expect("fixture cleans");
    }
}

#[test]
fn classic_output_trust_documentation_states_store_consistency_not_provenance_proof() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root resolves")
        .to_path_buf();
    for document in ["README.md", "telchar-design.md"] {
        let text = fs::read_to_string(repository.join(document)).expect("documentation reads");
        assert!(text.contains("store consistency"), "{document}");
        assert!(text.contains("trusted executor"), "{document}");
        assert!(text.contains("not provenance proof"), "{document}");
        assert!(!text.contains("cryptographically proven"), "{document}");
        assert!(!text.contains("reproducibly verified"), "{document}");
        assert!(!text.contains("builder-independent"), "{document}");
    }
}

#[test]
fn executor_streams_bounded_helper_logs_before_returning_result() {
    let root = unique_root("logs");
    fs::create_dir_all(&root).expect("fixture root creates");
    let helper = root.join("log-helper");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf 'first log\\n' >&2\nprintf 'second log\\n' >&2\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"{}\"]]}}\\n'\n",
            String::from_utf8_lossy(OUTPUT_PATH),
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper is executable");
    let build = admitted_request();
    let request = LocalExecutionRequest::new("logs", &build, Duration::from_secs(5))
        .expect("execution request is valid");
    let mut executor = NixStoreExecutor::new(&helper, "unix:///fixed-gateway.sock")
        .expect("executor config is valid");
    let mut logs = Vec::new();

    let result = executor
        .execute_with_logs(&request, &mut |chunk| {
            logs.extend_from_slice(chunk);
            Ok(())
        })
        .expect("fake helper succeeds");

    assert_eq!(result.status(), LocalBuildStatus::Built);
    assert_eq!(logs, b"first log\nsecond log\n");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn executor_streams_logs_larger_than_the_result_limit() {
    let root = unique_root("large-logs");
    fs::create_dir_all(&root).expect("fixture root creates");
    let helper = root.join("log-helper");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\nhead -c 131072 /dev/zero | tr '\\0' x >&2\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"{}\"]]}}\\n'\n",
            String::from_utf8_lossy(OUTPUT_PATH),
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper is executable");
    let build = admitted_request();
    let request = LocalExecutionRequest::new("large-logs", &build, Duration::from_secs(5))
        .expect("execution request is valid");
    let mut executor = NixStoreExecutor::new(&helper, "unix:///fixed-gateway.sock")
        .expect("executor config is valid");
    let mut log_bytes = 0;

    executor
        .execute_with_logs(&request, &mut |chunk| {
            log_bytes += chunk.len();
            Ok(())
        })
        .expect("large logs stream without retained-output failure");

    assert_eq!(log_bytes, 131072);
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn log_writer_failure_kills_and_reaps_the_helper() {
    let root = unique_root("log-writer-failure");
    fs::create_dir_all(&root).expect("fixture root creates");
    let helper = root.join("log-helper");
    let pid_path = root.join("pid");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > '{}'\ncat >/dev/null\nprintf 'build log\\n' >&2\nsleep 30\n",
            pid_path.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper is executable");
    let build = admitted_request();
    let request = LocalExecutionRequest::new("log-writer-failure", &build, Duration::from_secs(5))
        .expect("execution request is valid");
    let mut executor = NixStoreExecutor::new(&helper, "unix:///fixed-gateway.sock")
        .expect("executor config is valid");

    let error = executor
        .execute_with_logs(&request, &mut |_| {
            Err(std::io::Error::other("writer closed"))
        })
        .expect_err("log writer failure must fail execution");

    assert_eq!(error.to_string(), "writer closed");
    let pid = fs::read_to_string(&pid_path).expect("helper recorded PID");
    let status = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .expect("process liveness query runs");
    assert!(
        !status.success(),
        "helper remains alive after log writer failure"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn cancellation_kills_and_reaps_a_silent_helper() {
    let root = unique_root("cancelled");
    fs::create_dir_all(&root).expect("fixture root creates");
    let helper = root.join("cancelled-helper");
    let pid_path = root.join("pid");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > '{}'\ncat >/dev/null\nsleep 30\n",
            pid_path.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper is executable");
    let build = admitted_request();
    let request = LocalExecutionRequest::new("cancelled", &build, Duration::from_secs(5))
        .expect("execution request is valid");
    let mut executor = NixStoreExecutor::new(&helper, "unix:///fixed-gateway.sock")
        .expect("executor config is valid");

    let mut cancellation_checks = 0;
    let error = executor
        .execute_with_cancellation(&request, &mut |_| Ok(()), &mut || {
            cancellation_checks += 1;
            Ok(cancellation_checks > 1)
        })
        .expect_err("cancelled request must stop execution");

    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
    let pid = fs::read_to_string(&pid_path).expect("helper recorded PID");
    let status = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .expect("process liveness query runs");
    assert!(!status.success(), "cancelled helper remains alive");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn executor_rejects_malformed_success_output_sets() {
    for (label, response) in [
        (
            "missing",
            "{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[]}",
        ),
        (
            "extra",
            "{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-local-executor\"],[\"extra\",\"/nix/store/22222222222222222222222222222222-extra\"]]}",
        ),
        (
            "wrong-name",
            "{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"wrong\",\"/nix/store/11111111111111111111111111111111-telchar-local-executor\"]]}",
        ),
        (
            "wrong-path",
            "{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/22222222222222222222222222222222-wrong\"]]}",
        ),
        (
            "duplicate",
            "{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-local-executor\"],[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-local-executor\"]]}",
        ),
        (
            "unsupported-status",
            "{\"version\":1,\"success\":true,\"status\":\"substituted\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-local-executor\"]]}",
        ),
        (
            "helper-cannot-select-output-trust",
            "{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-local-executor\"]],\"trust\":\"provenance-proof\"}",
        ),
    ] {
        let root = unique_root(label);
        fs::create_dir_all(&root).expect("fixture root creates");
        let helper = root.join("hostile-helper");
        fs::write(
            &helper,
            format!("#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '%s\\n' '{response}'\n"),
        )
        .expect("helper writes");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))
            .expect("helper is executable");
        let build = admitted_request();
        let request = LocalExecutionRequest::new("hostile", &build, Duration::from_secs(5))
            .expect("execution request is valid");
        let mut executor = NixStoreExecutor::new(&helper, "unix:///fixed-gateway.sock")
            .expect("executor config is valid");

        let error = executor
            .execute(&request)
            .expect_err("{label} helper response must fail closed");
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::InvalidData,
            "{label}: {error}"
        );
        fs::remove_dir_all(root).expect("fixture cleans");
    }
}

#[test]
fn executor_rejects_oversized_or_malformed_helper_output() {
    for label in ["oversized", "malformed"] {
        let root = unique_root(label);
        fs::create_dir_all(&root).expect("fixture root creates");
        let helper = root.join("hostile-helper");
        fs::write(
            &helper,
            format!(
                "#!/bin/sh\nset -eu\ncat >/dev/null\n{}",
                if label == "oversized" {
                    "head -c 65537 /dev/zero | tr '\\0' x\n"
                } else {
                    "printf 'not-json\\n'\n"
                }
            ),
        )
        .expect("helper writes");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))
            .expect("helper is executable");
        let build = admitted_request();
        let request = LocalExecutionRequest::new("hostile", &build, Duration::from_secs(5))
            .expect("execution request is valid");
        let mut executor = NixStoreExecutor::new(&helper, "unix:///fixed-gateway.sock")
            .expect("executor config is valid");

        let error = executor
            .execute(&request)
            .expect_err("hostile helper output must fail closed");
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::Other
            ),
            "{label}: {error}"
        );
        fs::remove_dir_all(root).expect("fixture cleans");
    }
}

#[test]
fn executor_times_out_and_reaps_the_helper() {
    let root = unique_root("timeout");
    fs::create_dir_all(&root).expect("fixture root creates");
    let helper = root.join("slow-helper");
    let pid_path = root.join("pid");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > '{}'\ncat >/dev/null\nsleep 30\n",
            pid_path.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper is executable");
    let build = admitted_request();
    let request = LocalExecutionRequest::new("timeout", &build, Duration::from_millis(100))
        .expect("execution request is valid");
    let mut executor = NixStoreExecutor::new(&helper, "unix:///fixed-gateway.sock")
        .expect("executor config is valid");

    let error = executor
        .execute(&request)
        .expect_err("slow helper must time out");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    let pid = fs::read_to_string(&pid_path).expect("helper recorded PID");
    let status = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .expect("process liveness query runs");
    assert!(!status.success(), "timed-out helper remains alive");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn gateway_executor_rejects_zero_exit_when_expected_output_is_missing() {
    let fixture = NixFixture::create().expect("Nix fixture creates");
    let mut store = fixture
        .start_daemon(telchar::nix_fixture::TrustMode::Trusted)
        .expect("Nix daemon starts");
    let build = admitted_request_with_builder(b"exit 0");
    let request = LocalExecutionRequest::new("missing-output", &build, Duration::from_secs(30))
        .expect("execution request is valid");
    let endpoint = GatewayStoreEndpoint::parse(&store.store_url()).expect("endpoint parses");
    let mut executor = GatewayStoreExecutor::new(endpoint);

    let error = executor
        .execute(&request)
        .expect_err("zero-exit builder without output must fail execution");

    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    store.stop().expect("daemon stops");
    fixture.cleanup().expect("fixture cleans");
}

fn admitted_request() -> BuildRequest {
    admitted_request_with_builder(b"printf telchar-local-executor > $out")
}

fn admitted_request_with_builder(builder_command: &[u8]) -> BuildRequest {
    let mut wire = Vec::new();
    write_string(&mut wire, DERIVATION_PATH);
    write_integer(&mut wire, 1);
    write_string(&mut wire, b"out");
    write_string(&mut wire, OUTPUT_PATH);
    write_string(&mut wire, b"");
    write_string(&mut wire, b"");
    write_integer(&mut wire, 0);
    write_string(&mut wire, b"x86_64-linux");
    write_string(&mut wire, b"/bin/sh");
    write_integer(&mut wire, 2);
    write_string(&mut wire, b"-c");
    write_string(&mut wire, builder_command);
    write_integer(&mut wire, 4);
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"telchar-local-executor".as_slice()),
        (b"out".as_slice(), OUTPUT_PATH),
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

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "telchar-local-executor-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock follows epoch")
            .as_nanos()
    ))
}
