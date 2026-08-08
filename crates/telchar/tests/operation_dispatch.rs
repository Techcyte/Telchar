use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_ERROR, STDERR_LAST,
};
use telchar::nix_fixture::{NixFixture, TrustMode};

mod support;

use support::postgres::PostgresFixture;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
fn live_set_options_request_returns_terminal_frame() {
    let mut fixture = FrontendFixture::spawn(None);
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");

    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 19);
    for _ in 0..12 {
        write_integer(&mut input, 0);
    }
    write_integer(&mut input, 0);
    drop(input);

    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .expect("server stdout")
        .read_to_end(&mut stdout)
        .expect("server stdout reads");
    let mut expected_stdout = Vec::new();
    write_integer(&mut expected_stdout, SERVER_WORKER_MAGIC);
    write_integer(&mut expected_stdout, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut expected_stdout, 0);
    write_string(&mut expected_stdout, b"telchar");
    write_integer(&mut expected_stdout, 0);
    write_integer(&mut expected_stdout, STDERR_LAST);
    write_integer(&mut expected_stdout, STDERR_LAST);
    assert_eq!(
        stdout, expected_stdout,
        "worker stdout has no text contamination"
    );

    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.set_options.completed"),
        "missing local SetOptions telemetry: {stderr}"
    );
}

#[test]
fn partial_set_options_times_out_after_operation_and_cleans_up() {
    let mut fixture = FrontendFixture::spawn(Some(40));
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    input.flush().expect("handshake flushes");
    let mut output = child.stdout.take().expect("server output");
    assert_eq!(read_integer(&mut output), SERVER_WORKER_MAGIC);
    assert_eq!(read_integer(&mut output), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_integer(&mut output), 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().expect("post-handshake flushes");
    assert_eq!(read_string(&mut output), "telchar");
    assert_eq!(read_integer(&mut output), 0);
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    write_integer(&mut input, 19);
    input.flush().expect("operation flushes");
    let started = std::time::Instant::now();
    let elapsed = started.elapsed();
    let status = child.wait().expect("Telchar exits");
    assert!(elapsed < Duration::from_secs(1));
    assert!(status.success());
    let stderr = fixture.finish();
    assert!(stderr.contains("worker.session.timed_out"), "{stderr}");
}

#[test]
fn complete_message_boundary_remains_idle_until_next_input_starts() {
    let mut fixture = FrontendFixture::spawn(Some(40));
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    input.flush().expect("handshake flushes");
    let mut output = child.stdout.take().expect("server output");
    assert_eq!(read_integer(&mut output), SERVER_WORKER_MAGIC);
    assert_eq!(read_integer(&mut output), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_integer(&mut output), 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().expect("post-handshake flushes");
    assert_eq!(read_string(&mut output), "telchar");
    assert_eq!(read_integer(&mut output), 0);
    assert_eq!(read_integer(&mut output), STDERR_LAST);

    thread::sleep(Duration::from_millis(80));
    assert!(
        child.try_wait().expect("frontend status").is_none(),
        "complete-boundary idle session timed out"
    );

    input
        .write_all(&19_u64.to_le_bytes()[..1])
        .expect("partial operation starts");
    input.flush().expect("partial operation flushes");
    assert!(child.wait().expect("frontend exits").success());
    let stderr = fixture.finish();
    assert!(stderr.contains("worker.session.timed_out"), "{stderr}");
}

#[test]
fn partial_set_options_progress_resets_deadline() {
    let mut fixture = FrontendFixture::spawn(Some(40));
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    input.flush().expect("handshake flushes");
    let mut output = child.stdout.take().expect("server output");
    assert_eq!(read_integer(&mut output), SERVER_WORKER_MAGIC);
    assert_eq!(read_integer(&mut output), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_integer(&mut output), 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().expect("post-handshake flushes");
    assert_eq!(read_string(&mut output), "telchar");
    assert_eq!(read_integer(&mut output), 0);
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    write_integer(&mut input, 19);
    write_integer(&mut input, 0);
    input.flush().expect("partial request progresses");
    std::thread::sleep(Duration::from_millis(25));
    for _ in 0..11 {
        write_integer(&mut input, 0);
    }
    write_integer(&mut input, 0);
    input.flush().expect("request completes");
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    fixture.finish();
}

#[test]
fn query_valid_paths_returns_only_authoritative_valid_paths() {
    let fixture = NixFixture::create().expect("Nix fixture creates");
    let mut store = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("Nix daemon starts");
    let valid = store
        .build_classic_derivation()
        .expect("authoritative fixture path builds");
    let invalid = fixture
        .store_dir()
        .join("00000000000000000000000000000000-missing");
    let mut frontend =
        FrontendFixture::spawn_with_store(None, &store.store_url(), fixture.environment());
    let child = &mut frontend.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 31);
    write_integer(&mut input, 2);
    write_string(&mut input, valid.as_os_str().as_encoded_bytes());
    write_string(&mut input, invalid.as_os_str().as_encoded_bytes());
    write_integer(&mut input, 0);
    input.flush().expect("QueryValidPaths request flushes");

    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(read_integer(&mut output), 1, "one requested path is valid");
    assert_eq!(read_string(&mut output), valid.to_string_lossy());
    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    let stderr = frontend.finish();
    assert!(
        stderr.contains("worker.query_valid_paths.completed"),
        "missing completion telemetry: {stderr}"
    );

    store.stop().expect("Nix daemon stops");
    fixture.cleanup().expect("Nix fixture cleans");
}

#[test]
fn query_path_info_returns_authoritative_metadata() {
    let fixture = NixFixture::create().expect("Nix fixture creates");
    let mut store = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("Nix daemon starts");
    let valid = store
        .build_classic_derivation()
        .expect("authoritative fixture path builds");
    let mut frontend =
        FrontendFixture::spawn_with_store_export(None, &store.store_url(), fixture.environment());
    let child = &mut frontend.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 26);
    write_string(&mut input, valid.as_os_str().as_encoded_bytes());
    input.flush().expect("QueryPathInfo flushes");

    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(read_integer(&mut output), 1);
    let _deriver = read_string(&mut output);
    assert_eq!(read_string(&mut output).len(), 64);
    let reference_count = read_integer(&mut output);
    for _ in 0..reference_count {
        let _ = read_string(&mut output);
    }
    let _registration_time = read_integer(&mut output);
    assert!(read_integer(&mut output) > 0);
    let _ultimate = read_integer(&mut output);
    let signature_count = read_integer(&mut output);
    for _ in 0..signature_count {
        let _ = read_string(&mut output);
    }
    let _content_address = read_string(&mut output);

    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    let stderr = frontend.finish();
    assert!(
        stderr.contains("worker.query_path_info.completed"),
        "{stderr}"
    );
    store.stop().expect("daemon stops");
    fixture.cleanup().expect("fixture cleans");
}

#[test]
fn empty_query_valid_paths_returns_empty_set_without_store_lookup() {
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///definitely/missing/store.sock",
        std::iter::empty::<(&str, String)>(),
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 31);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input
        .flush()
        .expect("empty QueryValidPaths request flushes");

    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(
        read_integer(&mut output),
        0,
        "empty request has empty result"
    );
    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.query_valid_paths.completed"),
        "missing completion telemetry: {stderr}"
    );
}

#[test]
fn oversized_query_valid_paths_count_fails_before_reading_path_bodies() {
    let mut fixture = FrontendFixture::spawn(Some(100));
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 31);
    write_integer(&mut input, 257);
    input.flush().expect("oversized count flushes");

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(read_string(&mut output), "invalid QueryValidPaths request");
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(
        stderr.contains("invalid-query-valid-paths"),
        "missing bounded rejection evidence: {stderr}"
    );
    assert!(!stderr.contains("worker.session.timed_out"), "{stderr}");
}

#[test]
fn empty_add_multiple_to_store_completes_and_keeps_session_open() {
    let mut fixture = FrontendFixture::spawn(None);
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 44);
    write_integer(&mut input, 0);
    write_integer(&mut input, 1);
    write_integer(&mut input, 8);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().expect("empty upload batch flushes");

    assert_eq!(read_integer(&mut output), STDERR_LAST);

    write_integer(&mut input, 0xffff);
    input.flush().expect("next operation flushes");
    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(read_string(&mut output), "unknown worker operation");
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.add_multiple_to_store.completed"),
        "missing empty-batch completion telemetry: {stderr}"
    );
    assert!(
        stderr.contains("rejection=\"unknown-operation\""),
        "{stderr}"
    );
}

#[test]
fn incomplete_nonempty_add_multiple_to_store_fails_before_first_item_body() {
    let mut fixture = FrontendFixture::spawn(Some(100));
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 44);
    write_integer(&mut input, 0);
    write_integer(&mut input, 1);
    write_integer(&mut input, 8);
    write_integer(&mut input, 1);
    input.flush().expect("nonempty upload count flushes");

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(
        read_string(&mut output),
        "invalid AddMultipleToStore request"
    );
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);

    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(
        stderr.contains("invalid-add-multiple-to-store"),
        "missing fail-before-body evidence: {stderr}"
    );
    assert!(!stderr.contains("worker.session.timed_out"), "{stderr}");
}

#[test]
fn disk_reserve_rejects_transfer_before_nar_body_or_promotion() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-disk-import-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("promote-helper");
    let marker = root.join("helper-started");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf invoked > {}\n",
            marker.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [
            ("TELCHAR_NIX_STORE_PROMOTE", helper.display().to_string()),
            ("TELCHAR_GATEWAY_DISK_RESERVE_BYTES", u64::MAX.to_string()),
        ],
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_add_multiple_to_store_metadata(&mut input, 1);
    input.flush().expect("AddMultipleToStore metadata flushes");

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(
        read_string(&mut output),
        "gateway disk reserve check failed"
    );
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(!marker.exists(), "disk rejection started promotion helper");
    assert!(stderr.contains("worker.disk_reserve.rejected"), "{stderr}");
    assert!(stderr.contains("operation=\"transfer\""), "{stderr}");
    assert!(
        stderr.contains("reason=\"arithmetic-overflow\""),
        "{stderr}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn disk_reserve_rejects_build_before_helper_or_log_frame() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-disk-reserve-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let marker = root.join("helper-started");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf invoked > {}\nprintf 'unexpected-log\\n' >&2\n",
            marker.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [
            ("TELCHAR_NIX_STORE_BUILD", helper.display().to_string()),
            ("TELCHAR_GATEWAY_DISK_RESERVE_BYTES", u64::MAX.to_string()),
        ],
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_gate_3_build_derivation(&mut input, "x86_64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(read_string(&mut output), "gateway disk reserve exceeded");
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(!marker.exists(), "disk rejection started the build helper");
    assert!(stderr.contains("worker.disk_reserve.rejected"), "{stderr}");
    assert!(stderr.contains("operation=\"build\""), "{stderr}");
    assert!(stderr.contains("filesystem=\"gateway-store\""), "{stderr}");
    assert!(stderr.contains("reason=\"insufficient-space\""), "{stderr}");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn build_derivation_streams_helper_logs_before_success_result() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-log-helper-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    fs::write(
        &helper,
        "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf 'build-log-line\\n' >&2\nprintf '{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}\\n'\n",
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [("TELCHAR_NIX_STORE_BUILD", helper.display().to_string())],
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_gate_3_build_derivation(&mut input, "x86_64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");

    assert_eq!(read_integer(&mut output), nix_worker_protocol::STDERR_NEXT);
    assert_eq!(read_string(&mut output), "build-log-line\n");
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(read_integer(&mut output), 0, "Built status");
    assert_eq!(read_string(&mut output), "", "empty build error message");
    assert_eq!(read_integer(&mut output), 0, "times built");
    assert_eq!(read_integer(&mut output), 0, "not nondeterministic");
    assert_eq!(read_integer(&mut output), 0, "start time");
    assert_eq!(read_integer(&mut output), 0, "stop time");
    assert_eq!(read_integer(&mut output), 0, "no user CPU duration");
    assert_eq!(read_integer(&mut output), 0, "no system CPU duration");
    assert_eq!(read_integer(&mut output), 0, "no CA realisations");
    drop(input);
    drop(output);

    let status = child.wait().expect("Telchar exits");
    let stderr = fixture.finish();
    assert!(status.success(), "Telchar failed with {status}: {stderr}");
    assert!(
        stderr.contains("worker.build_derivation.completed"),
        "{stderr}"
    );
    assert!(!stderr.contains("build-log-line"), "{stderr}");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn disconnected_frontend_cancels_and_reaps_silent_build_helper() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-cancel-helper-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let pid_path = root.join("pid");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > '{}'\ncat >/dev/null\nsleep 30\n",
            pid_path.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [("TELCHAR_NIX_STORE_BUILD", helper.display().to_string())],
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);
    write_gate_3_build_derivation(&mut input, "x86_64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !pid_path.exists() {
        assert!(Instant::now() < deadline, "helper did not record PID");
        thread::sleep(Duration::from_millis(5));
    }

    child.kill().expect("frontend terminates");
    child.wait().expect("frontend reaps");
    drop(input);
    drop(output);

    let pid = fs::read_to_string(&pid_path).expect("helper recorded PID");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let alive = Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("process liveness query runs")
            .success();
        if !alive {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "disconnected helper remains alive"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.build_derivation.failed"),
        "{stderr}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn valid_build_derivation_is_consumed_before_execution_unavailable_error() {
    let mut fixture = FrontendFixture::spawn(None);
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_gate_3_build_derivation(&mut input, "x86_64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(
        read_string(&mut output),
        "BuildDerivation execution is unavailable"
    );
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.build_derivation.admitted"),
        "{stderr}"
    );
    assert!(
        stderr.contains("worker.build_derivation.execution_unavailable"),
        "{stderr}"
    );
    assert!(!stderr.contains("printf telchar-remote-build"), "{stderr}");
}

#[test]
fn mismatched_build_derivation_system_is_rejected_before_execution() {
    let mut fixture = FrontendFixture::spawn(None);
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_gate_3_build_derivation(&mut input, "aarch64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(
        read_string(&mut output),
        "unsupported BuildDerivation request"
    );
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(stderr.contains("unsupported-build-derivation"), "{stderr}");
    assert!(
        !stderr.contains("worker.build_derivation.admitted"),
        "{stderr}"
    );
}

#[test]
fn recognized_unsupported_operation_returns_a_distinct_framed_error() {
    let response = send_operation(39);
    assert_eq!(response.message, "unsupported worker operation");
    assert_eq!(response.rejection, "recognized-unsupported");
}

#[test]
fn unknown_operation_returns_a_framed_error() {
    let response = send_operation(0xffff);
    assert_eq!(response.message, "unknown worker operation");
    assert_eq!(response.rejection, "unknown-operation");
}

struct OperationResponse {
    message: String,
    rejection: &'static str,
}

struct FrontendFixture {
    root: PathBuf,
    frontend: Child,
    daemon: Child,
    _database: PostgresFixture,
}

impl FrontendFixture {
    fn spawn(worker_timeout_ms: Option<u64>) -> Self {
        Self::spawn_configured(
            worker_timeout_ms,
            None,
            std::iter::empty::<(&str, String)>(),
        )
    }

    fn spawn_with_store(
        worker_timeout_ms: Option<u64>,
        store_uri: &str,
        environment: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        Self::spawn_configured(worker_timeout_ms, Some(store_uri), environment)
    }

    fn spawn_with_store_export(
        worker_timeout_ms: Option<u64>,
        store_uri: &str,
        environment: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        let mut environment = environment.into_iter().collect::<Vec<_>>();
        environment.push((
            "TELCHAR_NIX_STORE_EXPORT",
            std::env::var("TELCHAR_NIX_STORE_EXPORT")
                .expect("flake-built export helper is configured"),
        ));
        environment.push((
            "TELCHAR_NIX",
            std::env::var("TELCHAR_NIX_BIN").expect("flake-pinned Nix is configured"),
        ));
        Self::spawn_configured(worker_timeout_ms, Some(store_uri), environment)
    }

    fn spawn_configured(
        worker_timeout_ms: Option<u64>,
        store_uri: Option<&str>,
        environment: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        let root = std::env::temp_dir().join(format!(
            "telchar-operation-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time follows epoch")
                .as_nanos(),
            FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("fixture root creates");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("fixture root permissions set");
        let socket = root.join("daemon.sock");
        let database = PostgresFixture::start();
        let mut daemon_command = Command::new(env!("CARGO_BIN_EXE_telchar"));
        daemon_command
            .args([
                "daemon",
                "--socket",
                socket.to_str().expect("UTF-8 socket path"),
                "--frontend-uid",
                &rustix::process::getuid().as_raw().to_string(),
                "--once",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        daemon_command
            .env("TELCHAR_DATABASE_URL", database.url())
            .env("TELCHAR_SYSTEM", "x86_64-linux")
            .env("TELCHAR_SUPPORTED_FEATURES", "")
            .env_remove("TELCHAR_NIX_STORE_BUILD")
            .env_remove("TELCHAR_NIX_STORE_EXPORT")
            .env_remove("TELCHAR_NIX_STORE_PROMOTE");
        if let Some(timeout) = worker_timeout_ms {
            daemon_command.env("TELCHAR_WORKER_IDLE_TIMEOUT_MS", timeout.to_string());
        }
        if let Some(store_uri) = store_uri {
            daemon_command.env("TELCHAR_GATEWAY_STORE_URI", store_uri);
        }
        daemon_command.envs(environment);
        let mut daemon = daemon_command.spawn().expect("daemon starts");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "daemon socket was not created");
            assert!(
                daemon.try_wait().expect("daemon status").is_none(),
                "daemon exited before binding"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_telchar"));
        command
            .arg("serve-stdio")
            .env("TELCHAR_IPC_SOCKET", &socket)
            .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:fixture")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(timeout) = worker_timeout_ms {
            command.env("TELCHAR_WORKER_IDLE_TIMEOUT_MS", timeout.to_string());
        }
        let frontend = command.spawn().expect("frontend starts");
        Self {
            root,
            frontend,
            daemon,
            _database: database,
        }
    }

    fn finish(mut self) -> String {
        let mut frontend_stderr = String::new();
        self.frontend
            .stderr
            .take()
            .expect("frontend stderr")
            .read_to_string(&mut frontend_stderr)
            .expect("frontend stderr reads");
        let daemon_output = self.daemon.wait_with_output().expect("daemon exits");
        let _ = fs::remove_dir_all(self.root);
        assert!(
            daemon_output.status.success(),
            "daemon failed: {daemon_output:?}"
        );
        format!(
            "{frontend_stderr}{}",
            String::from_utf8_lossy(&daemon_output.stderr)
        )
    }
}

fn send_operation(operation: u64) -> OperationResponse {
    let mut fixture = FrontendFixture::spawn(None);
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, operation);
    input.flush().expect("operation flushes");
    drop(input);

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    let message = read_string(&mut output);
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");

    let status = child.wait().expect("Telchar exits");
    assert!(status.success(), "Telchar failed: {status}");
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.operation.rejected"),
        "missing structured rejection event: {stderr}"
    );
    let rejection = if stderr.contains("recognized-unsupported") {
        "recognized-unsupported"
    } else {
        "unknown-operation"
    };
    OperationResponse { message, rejection }
}

fn complete_handshake(input: &mut impl Write, output: &mut impl Read) {
    write_integer(input, CLIENT_WORKER_MAGIC);
    write_integer(input, LATEST_WORKER_VERSION.to_wire());
    write_integer(input, 0);
    input.flush().expect("handshake flushes");

    assert_eq!(read_integer(output), SERVER_WORKER_MAGIC);
    assert_eq!(
        read_integer(output),
        LATEST_WORKER_VERSION.to_wire(),
        "server sends its protocol version"
    );
    assert_eq!(read_integer(output), 0, "server has no features");

    write_integer(input, 0);
    write_integer(input, 0);
    input.flush().expect("post-handshake flushes");

    assert_eq!(read_string(output), "telchar");
    assert_eq!(read_integer(output), 0);
    assert_eq!(read_integer(output), STDERR_LAST);
}

fn write_integer(output: &mut impl Write, value: u64) {
    output
        .write_all(&value.to_le_bytes())
        .expect("worker integer writes");
}

fn write_add_multiple_to_store_metadata(output: &mut impl Write, nar_size: u64) {
    let mut metadata = Vec::new();
    write_integer(&mut metadata, 1);
    write_string(
        &mut metadata,
        b"/nix/store/11111111111111111111111111111111-telchar-disk-reserve",
    );
    write_string(&mut metadata, b"");
    write_string(&mut metadata, b"");
    write_integer(&mut metadata, 0);
    write_integer(&mut metadata, 0);
    write_integer(&mut metadata, nar_size);
    write_integer(&mut metadata, 0);
    write_integer(&mut metadata, 0);
    write_string(&mut metadata, b"");

    write_integer(output, 44);
    write_integer(output, 0);
    write_integer(output, 0);
    write_integer(output, metadata.len() as u64);
    output.write_all(&metadata).expect("metadata frame writes");
}

fn write_gate_3_build_derivation(output: &mut impl Write, system: &str, mode: u64) {
    let store_output = b"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract";
    write_integer(output, 36);
    write_string(
        output,
        b"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv",
    );
    write_integer(output, 1);
    write_string(output, b"out");
    write_string(output, store_output);
    write_string(output, b"");
    write_string(output, b"");
    write_integer(output, 0);
    write_string(output, system.as_bytes());
    write_string(output, b"/bin/sh");
    write_integer(output, 2);
    write_string(output, b"-c");
    write_string(output, b"printf telchar-remote-build > $out");
    write_integer(output, 4);
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"telchar-gate-3-contract".as_slice()),
        (b"out".as_slice(), store_output.as_slice()),
        (b"system".as_slice(), system.as_bytes()),
    ] {
        write_string(output, key);
        write_string(output, value);
    }
    write_integer(output, mode);
}

fn write_string(output: &mut impl Write, value: &[u8]) {
    write_integer(output, value.len() as u64);
    output.write_all(value).expect("worker string writes");
    output
        .write_all(&[0; 7][..(8 - value.len() % 8) % 8])
        .expect("worker string padding writes");
}

fn read_integer(input: &mut impl Read) -> u64 {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes).expect("worker integer reads");
    u64::from_le_bytes(bytes)
}

fn read_string(input: &mut impl Read) -> String {
    let length = read_integer(input) as usize;
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes).expect("worker string reads");
    let padding = (8 - length % 8) % 8;
    let mut padding_bytes = vec![0; padding];
    input
        .read_exact(&mut padding_bytes)
        .expect("worker padding reads");
    assert!(padding_bytes.iter().all(|byte| *byte == 0));
    String::from_utf8(bytes).expect("worker string UTF-8")
}
