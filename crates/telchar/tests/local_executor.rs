use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use nix_worker_protocol::{ProtocolSessionLimits, WorkerReader};
use telchar::build_request::BuildRequest;
use telchar::deployment::DeploymentConfig;
use telchar::local_executor::{LocalBuildStatus, LocalExecutionRequest, NixStoreExecutor};
use telchar::nix_fixture::NixFixture;

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
        .status()
        .expect("process liveness query runs");
    assert!(!status.success(), "timed-out helper remains alive");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn flake_built_helper_executes_one_basic_derivation_in_the_gateway_store() {
    let fixture = NixFixture::create().expect("Nix fixture creates");
    let store_uri = format!("local?root={}", fixture.root().display());
    let build = admitted_request();
    let request = LocalExecutionRequest::new("real-build", &build, Duration::from_secs(30))
        .expect("execution request is valid");
    let mut executor =
        NixStoreExecutor::new(helper_path(), &store_uri).expect("executor config is valid");

    let result = executor.execute(&request).expect("real build succeeds");

    assert!(matches!(
        result.status(),
        LocalBuildStatus::Built | LocalBuildStatus::AlreadyValid
    ));
    assert_eq!(result.outputs(), &[(b"out".to_vec(), OUTPUT_PATH.to_vec())]);
    assert!(std::process::Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "--store",
            &store_uri,
            "path-info",
            std::str::from_utf8(OUTPUT_PATH).expect("output path is UTF-8"),
        ])
        .status()
        .expect("path validity query runs")
        .success());
    let real_output = fixture
        .root()
        .join("nix/store")
        .join(String::from_utf8_lossy(OUTPUT_PATH).trim_start_matches("/nix/store/"));
    assert_eq!(
        fs::read(real_output).expect("output reads"),
        b"telchar-local-executor"
    );
}

fn admitted_request() -> BuildRequest {
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
    write_string(&mut wire, b"printf telchar-local-executor > $out");
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

fn helper_path() -> PathBuf {
    std::env::var_os("TELCHAR_NIX_STORE_BUILD")
        .map(PathBuf::from)
        .expect("TELCHAR_NIX_STORE_BUILD points to the flake-built helper")
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
