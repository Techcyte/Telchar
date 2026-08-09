use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
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
#[ignore = "private fixture paths are outside the production /nix/store namespace"]
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
#[ignore = "private fixture paths are outside the production /nix/store namespace"]
fn query_path_info_returns_authoritative_metadata() {
    let fixture = NixFixture::create().expect("Nix fixture creates");
    let mut store = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("Nix daemon starts");
    let valid = store
        .build_classic_derivation()
        .expect("authoritative fixture path builds");
    let mut frontend =
        FrontendFixture::spawn_with_store(None, &store.store_url(), fixture.environment());
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
fn partial_add_multiple_to_store_failure_removes_staging_state() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-upload-disconnect-{}-{}",
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
    let staging_root = root.join("staging");
    fs::create_dir(&staging_root).expect("staging root creates");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [
            ("TELCHAR_NIX_STORE_PROMOTE", helper.display().to_string()),
            ("TMPDIR", staging_root.display().to_string()),
        ],
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_add_multiple_to_store_metadata(&mut input, 1024);
    input.write_all(b"partial-nar").expect("partial NAR writes");
    input.flush().expect("partial upload flushes");
    drop(input);
    drop(output);

    assert!(
        !child.wait().expect("Telchar exits").success(),
        "disconnected partial upload reported protocol success"
    );
    let mut frontend_stderr = String::new();
    fixture
        .frontend
        .stderr
        .take()
        .expect("frontend stderr")
        .read_to_string(&mut frontend_stderr)
        .expect("frontend stderr reads");
    let daemon_output = fixture.daemon.wait_with_output().expect("daemon exits");
    let stderr = format!(
        "{frontend_stderr}{}",
        String::from_utf8_lossy(&daemon_output.stderr)
    );
    assert!(!marker.exists(), "partial upload started promotion helper");
    assert_eq!(
        fs::read_dir(&staging_root)
            .expect("staging root reads")
            .count(),
        0,
        "partial upload retained staging state"
    );
    assert!(stderr.contains("invalid-add-multiple-to-store"), "{stderr}");
    fs::remove_dir_all(root).expect("fixture cleans");
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
    assert_eq!(
        fixture
            .database
            .connect()
            .query_one("SELECT count(*) FROM build_requests", &[])
            .expect("request count reads")
            .get::<_, i64>(0),
        0,
        "disk rejection persisted a build request"
    );
    let stderr = fixture.finish();
    assert!(!marker.exists(), "disk rejection started the build helper");
    assert!(stderr.contains("worker.disk_reserve.rejected"), "{stderr}");
    assert!(stderr.contains("operation=\"build\""), "{stderr}");
    assert!(stderr.contains("filesystem=\"gateway-store\""), "{stderr}");
    assert!(stderr.contains("reason=\"insufficient-space\""), "{stderr}");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn derivation_lease_precedes_helper_execution() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-derivation-lease-order-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let request_path = root.join("request");
    let started = root.join("helper-started");
    let complete = root.join("complete-helper");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat > '{}'\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            request_path.display(),
            started.display(),
            complete.display(),
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
    while !started.exists() {
        assert!(Instant::now() < deadline, "helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    let helper_request: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&request_path).expect("helper request reads"))
            .expect("helper request is JSON");
    let request_id = helper_request["request_id"]
        .as_str()
        .expect("helper request ID is a string");
    let lease = fixture
        .database
        .connect()
        .query_opt(
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state FROM store_leases WHERE owner_id = $1",
            &[&request_id],
        )
        .expect("lease query succeeds")
        .expect("active derivation lease exists before helper execution");
    assert!(lease.get::<_, String>(0).starts_with("lease-"));
    assert_eq!(lease.get::<_, String>(1), "request");
    assert_eq!(lease.get::<_, String>(2), request_id);
    assert_eq!(
        lease.get::<_, String>(3),
        "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"
    );
    assert_eq!(lease.get::<_, String>(4), "derivation");
    assert_eq!(lease.get::<_, String>(5), "active");
    let gc_root = fixture
        .root
        .join("gc-roots")
        .join(lease.get::<_, String>(0));
    assert!(
        fs::symlink_metadata(&gc_root)
            .expect("derivation root metadata reads")
            .file_type()
            .is_symlink(),
        "derivation root missing before helper execution"
    );

    fs::write(&complete, b"complete").expect("helper completion releases");
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(read_integer(&mut output), 0, "Built status");
    assert_eq!(read_string(&mut output), "", "empty build error message");
    for _ in 0..7 {
        read_integer(&mut output);
    }
    drop(input);
    drop(output);
    assert!(child.wait().expect("Telchar exits").success());
    fixture.finish();
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn input_roots_precede_atomic_input_lease_commit_and_helper_execution() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-input-root-order-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let socket = root.join("gateway.sock");
    let closure_daemon = spawn_closure_daemon(&socket, true);
    let started = root.join("helper-started");
    let complete = root.join("complete-helper");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            started.display(),
            complete.display(),
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        &format!("unix://{}", socket.display()),
        [("TELCHAR_NIX_STORE_BUILD", helper.display().to_string())],
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);
    write_input_build_derivation(&mut input, "x86_64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");

    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        assert!(Instant::now() < deadline, "helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    let mut database = fixture.database.connect();
    let input_leases = database
        .query(
            "SELECT lease_id, store_path FROM store_leases WHERE purpose = 'input' ORDER BY lease_id",
            &[],
        )
        .expect("input leases read");
    assert_eq!(input_leases.len(), 1, "input lease commits before helper");
    let lease_id = input_leases[0].get::<_, String>(0);
    assert_eq!(
        input_leases[0].get::<_, String>(1),
        "/nix/store/22222222222222222222222222222222-telchar-input"
    );
    let root_path = fixture.root.join("gc-roots").join(lease_id);
    assert!(
        fs::symlink_metadata(root_path)
            .expect("input root metadata reads")
            .file_type()
            .is_symlink(),
        "input root missing before atomic input lease commit"
    );

    fs::write(&complete, b"complete").expect("helper completion releases");
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(read_integer(&mut output), 0, "Built status");
    assert_eq!(read_string(&mut output), "", "empty build error message");
    for _ in 0..7 {
        read_integer(&mut output);
    }
    drop(input);
    drop(output);
    assert!(child.wait().expect("Telchar exits").success());
    fixture.finish();
    closure_daemon.join().expect("closure daemon exits");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn input_lease_persistence_failure_rolls_back_input_roots() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-input-lease-failure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let socket = root.join("gateway.sock");
    let closure_daemon = spawn_closure_daemon(&socket, false);
    let marker = root.join("helper-started");
    fs::write(
        &helper,
        format!("#!/bin/sh\nprintf invoked > '{}'\n", marker.display()),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        &format!("unix://{}", socket.display()),
        [("TELCHAR_NIX_STORE_BUILD", helper.display().to_string())],
    );
    fixture
        .database
        .connect()
        .batch_execute(
            "CREATE FUNCTION reject_input_lease_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.purpose = 'input' THEN RAISE EXCEPTION 'reject input lease insert'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_input_lease_insert BEFORE INSERT ON store_leases FOR EACH ROW EXECUTE FUNCTION reject_input_lease_insert();",
        )
        .expect("input failure trigger installs");
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);
    write_input_build_derivation(&mut input, "x86_64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(
        read_string(&mut output),
        "store lease state operation failed"
    );
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    assert!(!marker.exists(), "input lease failure started helper");
    let mut database = fixture.database.connect();
    assert_eq!(
        database
            .query_one(
                "SELECT count(*) FROM store_leases WHERE purpose = 'derivation' AND state = 'released'",
                &[]
            )
            .expect("released derivation lease count reads")
            .get::<_, i64>(0),
        1,
        "input lease failure retained an active derivation lease"
    );
    assert_eq!(
        database
            .query_one(
                "SELECT count(*) FROM store_leases WHERE purpose = 'input'",
                &[]
            )
            .expect("input lease count reads")
            .get::<_, i64>(0),
        0,
        "input lease failure persisted an input lease"
    );
    assert_eq!(
        fs::read_dir(fixture.root.join("gc-roots"))
            .expect("GC root directory reads")
            .count(),
        0,
        "input lease failure retained a terminal request root"
    );
    let stderr = fixture.finish();
    let retention_events = stderr
        .lines()
        .filter(|line| line.contains("event=\"gateway.store_retention\""))
        .collect::<Vec<_>>();
    assert!(
        retention_events.iter().any(|line| {
            line.contains("operation=\"retain\"")
                && line.contains("purpose=\"input\"")
                && line.contains("path_count=1")
                && line.contains("result=\"succeeded\"")
        }) && retention_events.iter().any(|line| {
            line.contains("operation=\"rollback\"")
                && line.contains("purpose=\"input\"")
                && line.contains("path_count=1")
                && line.contains("result=\"succeeded\"")
        }),
        "{stderr}"
    );
    for event in retention_events {
        assert!(!event.contains("lease-"), "{event}");
        assert!(!event.contains("/nix/store/"), "{event}");
        assert!(!event.contains("gc-roots"), "{event}");
    }
    closure_daemon.join().expect("closure daemon exits");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn build_request_attachment_precedes_helper_and_detaches_after_response() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-attachment-order-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let request_path = root.join("request-id");
    let started = root.join("helper-started");
    let complete = root.join("complete-helper");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat > '{}'\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            request_path.display(),
            started.display(),
            complete.display(),
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
    while !started.exists() {
        assert!(Instant::now() < deadline, "helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    let helper_request: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&request_path).expect("helper request reads"))
            .expect("helper request is JSON");
    let request_id = helper_request["request_id"]
        .as_str()
        .expect("helper request ID is a string");
    let session_id = fixture
        .database
        .connect()
        .query_one(
            "SELECT session_id FROM request_attachments WHERE request_id = $1",
            &[&request_id],
        )
        .expect("attachment exists before helper result")
        .get::<_, String>(0);
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.database.url(),
            &session_id,
            request_id
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Attached
    );
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.database.url(), &session_id)
            .expect("session reads")
            .expect("session exists")
            .state,
        telchar::persistence::ProtocolSessionState::Open
    );
    assert_active_derivation_lease(&fixture.database, request_id);

    fs::write(&complete, b"complete").expect("helper completion releases");
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(read_integer(&mut output), 0, "Built status");
    assert_eq!(read_string(&mut output), "", "empty build error message");
    for _ in 0..7 {
        read_integer(&mut output);
    }
    drop(input);
    drop(output);
    assert!(child.wait().expect("Telchar exits").success());
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.database.url(),
            &session_id,
            request_id
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Detached
    );
    assert_released_derivation_lease(&fixture.database, request_id);
    let mut database = fixture.database.connect();
    let output_leases = database
        .query(
            "SELECT lease_id, store_path, purpose, state FROM store_leases WHERE owner_id = $1 AND purpose = 'output' ORDER BY lease_id",
            &[&request_id],
        )
        .expect("output leases read");
    assert_eq!(
        output_leases.len(),
        1,
        "successful build has one output lease"
    );
    let output_lease_id = output_leases[0].get::<_, String>(0);
    assert_eq!(
        output_leases[0].get::<_, String>(1),
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"
    );
    assert_eq!(output_leases[0].get::<_, String>(2), "output");
    assert_eq!(output_leases[0].get::<_, String>(3), "active");
    assert_eq!(
        fs::read_link(fixture.root.join("gc-roots").join(output_lease_id))
            .expect("output root reads"),
        PathBuf::from("/nix/store/11111111111111111111111111111111-telchar-gate-3-contract")
    );
    assert_eq!(
        database
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = $1 AND purpose IN ('derivation', 'input') AND state = 'active'",
                &[&request_id],
            )
            .expect("active request lease count reads")
            .get::<_, i64>(0),
        0,
        "successful cleanup retained derivation or input leases"
    );
    let stderr = fixture.finish();
    assert!(!stderr.contains(&session_id), "{stderr}");
    assert!(!stderr.contains(request_id), "{stderr}");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn missing_expected_output_fails_before_result_and_releases_request_state() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-missing-output-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let request_path = root.join("request.json");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat > '{}'\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[]}}\\n'\n",
            request_path.display()
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

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(read_string(&mut output), "BuildDerivation execution failed");
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);
    assert!(child.wait().expect("Telchar exits").success());

    let helper_request: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&request_path).expect("helper request reads"))
            .expect("helper request is JSON");
    let request_id = helper_request["request_id"]
        .as_str()
        .expect("helper request ID is a string");
    let mut database = fixture.database.connect();
    let session_id = database
        .query_one(
            "SELECT session_id FROM request_attachments WHERE request_id = $1",
            &[&request_id],
        )
        .expect("attachment reads")
        .get::<_, String>(0);
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.database.url(),
            &session_id,
            request_id
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Detached
    );
    assert_released_derivation_lease(&fixture.database, request_id);
    assert_eq!(
        database
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = $1 AND state = 'active'",
                &[&request_id],
            )
            .expect("active lease count reads")
            .get::<_, i64>(0),
        0,
        "missing output left active request leases"
    );
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.build_derivation.failed"),
        "{stderr}"
    );
    assert!(!stderr.contains(request_id), "{stderr}");
    assert!(!stderr.contains(&session_id), "{stderr}");
    assert!(
        !stderr.contains("/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"),
        "{stderr}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn invalid_output_metadata_fails_before_result_and_releases_request_state() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-invalid-output-metadata-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let build_helper = root.join("build-helper");
    fs::write(
        &build_helper,
        "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}\\n'\n",
    )
    .expect("build helper writes");
    fs::set_permissions(&build_helper, fs::Permissions::from_mode(0o700))
        .expect("build helper executable");
    let nar_path = root.join("output.nar");
    fs::write(&nar_path, regular_nar(b"telchar-output-metadata-secret"))
        .expect("output NAR writes");
    let export_helper = root.join("export-helper");
    fs::write(
        &export_helper,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\ncat '{}'\n",
            nar_path.display()
        ),
    )
    .expect("export helper writes");
    fs::set_permissions(&export_helper, fs::Permissions::from_mode(0o700))
        .expect("export helper executable");
    let nix = root.join("nix");
    fs::write(
        &nix,
        "#!/bin/sh\nset -eu\nprintf '{\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\":{\"narHash\":\"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\",\"narSize\":136,\"references\":[],\"deriver\":null,\"ca\":null}}\\n'\n",
    )
    .expect("Nix query helper writes");
    fs::set_permissions(&nix, fs::Permissions::from_mode(0o700)).expect("Nix helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [
            (
                "TELCHAR_NIX_STORE_BUILD",
                build_helper.display().to_string(),
            ),
            (
                "TELCHAR_NIX_STORE_EXPORT",
                export_helper.display().to_string(),
            ),
            ("TELCHAR_NIX", nix.display().to_string()),
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
    assert_eq!(read_string(&mut output), "BuildDerivation execution failed");
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);
    assert!(child.wait().expect("Telchar exits").success());

    let mut database = fixture.database.connect();
    let request_id = request_id(&mut database);
    let session_id = database
        .query_one(
            "SELECT session_id FROM request_attachments WHERE request_id = $1",
            &[&request_id],
        )
        .expect("attachment reads")
        .get::<_, String>(0);
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.database.url(),
            &session_id,
            &request_id
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Detached
    );
    assert_released_derivation_lease(&fixture.database, &request_id);
    assert_eq!(
        database
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = $1 AND state = 'active'",
                &[&request_id],
            )
            .expect("active lease count reads")
            .get::<_, i64>(0),
        0,
        "invalid output metadata left active request leases"
    );
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.build_derivation.output_validation_failed"),
        "{stderr}"
    );
    assert!(!stderr.contains(&request_id), "{stderr}");
    assert!(!stderr.contains(&session_id), "{stderr}");
    assert!(
        !stderr.contains("telchar-output-metadata-secret"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"),
        "{stderr}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn output_lease_failure_rolls_back_output_root_before_request_cleanup() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-output-lease-failure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    fs::write(
        &helper,
        "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}\\n'\n",
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [("TELCHAR_NIX_STORE_BUILD", helper.display().to_string())],
    );
    fixture
        .database
        .connect()
        .batch_execute(
            "CREATE FUNCTION reject_output_lease() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.purpose = 'output' THEN RAISE EXCEPTION 'reject output lease'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_output_lease BEFORE INSERT ON store_leases FOR EACH ROW EXECUTE FUNCTION reject_output_lease();",
        )
        .expect("output lease failure trigger installs");
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
        "store lease state operation failed"
    );
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);
    assert!(child.wait().expect("Telchar exits").success());

    let mut database = fixture.database.connect();
    let request_id = request_id(&mut database);
    assert_eq!(
        database
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = $1 AND state = 'active'",
                &[&request_id],
            )
            .expect("active lease count reads")
            .get::<_, i64>(0),
        0,
        "output lease failure left active request leases"
    );
    assert_eq!(
        fs::read_dir(fixture.root.join("gc-roots"))
            .expect("GC root directory reads")
            .count(),
        0,
        "output lease failure left a request root"
    );
    let stderr = fixture.finish();
    assert!(
        stderr.contains("operation=\"create-output-retention\""),
        "{stderr}"
    );
    assert!(!stderr.contains(&request_id), "{stderr}");
    assert!(
        !stderr.contains("/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"),
        "{stderr}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn detach_failure_does_not_send_successful_build_result() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-detach-failure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    fs::write(
        &helper,
        "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}\\n'\n",
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [("TELCHAR_NIX_STORE_BUILD", helper.display().to_string())],
    );
    fixture
        .database
        .connect()
        .batch_execute(
            "CREATE FUNCTION reject_attachment_detach() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject detach'; END $$; CREATE TRIGGER reject_attachment_detach BEFORE UPDATE ON request_attachments FOR EACH ROW EXECUTE FUNCTION reject_attachment_detach();",
        )
        .expect("detach failure trigger installs");
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);
    write_gate_3_build_derivation(&mut input, "x86_64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");
    drop(input);

    let mut response = Vec::new();
    output
        .read_to_end(&mut response)
        .expect("response stream closes");
    assert!(
        !response.is_empty(),
        "detach failure sent no terminal error"
    );
    let mut response = response.as_slice();
    assert_eq!(read_integer(&mut response), STDERR_ERROR);
    assert_eq!(read_string(&mut response), "Error");
    let _level = read_integer(&mut response);
    assert_eq!(read_string(&mut response), "Error");
    assert_eq!(
        read_string(&mut response),
        "store lease state operation failed"
    );
    assert_eq!(read_integer(&mut response), 0, "error has no position");
    assert_eq!(read_integer(&mut response), 0, "error has no trace");
    assert!(response.is_empty(), "terminal error has trailing bytes");
    assert!(child.wait().expect("Telchar exits").success());
    assert_eq!(
        fixture
            .database
            .connect()
            .query_one("SELECT state FROM request_attachments", &[])
            .expect("attachment state reads")
            .get::<_, String>(0),
        "attached",
        "failed detach changed attachment state"
    );
    let stderr = fixture.finish();
    assert!(
        stderr.contains("database.request_lease_release.failed"),
        "{stderr}"
    );
    assert!(stderr.contains("operation=\"detach-release\""), "{stderr}");
    assert!(!stderr.contains("reject detach"), "{stderr}");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn root_release_failure_reports_retention_error_after_durable_release() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-root-release-failure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let started = root.join("helper-started");
    let complete = root.join("complete-helper");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            started.display(),
            complete.display(),
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
    while !started.exists() {
        assert!(Instant::now() < deadline, "helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    let mut client = fixture.database.connect();
    let lease_id = client
        .query_one(
            "SELECT lease_id FROM store_leases WHERE purpose = 'derivation'",
            &[],
        )
        .expect("derivation lease reads")
        .get::<_, String>(0);
    let gc_root = fixture.root.join("gc-roots").join(lease_id);
    fs::remove_file(&gc_root).expect("root removes");
    let conflicting_target = root.join("conflicting-target");
    std::os::unix::fs::symlink(&conflicting_target, &gc_root).expect("conflicting root creates");
    fs::write(&complete, b"complete").expect("helper completion releases");

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(read_string(&mut output), "gateway store retention failed");
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    assert_eq!(
        client
            .query_one("SELECT state FROM request_attachments", &[])
            .expect("attachment state reads")
            .get::<_, String>(0),
        "detached"
    );
    assert_eq!(
        client
            .query_one(
                "SELECT state FROM store_leases WHERE purpose = 'derivation'",
                &[],
            )
            .expect("lease state reads")
            .get::<_, String>(0),
        "released"
    );
    assert_eq!(
        fs::read_link(&gc_root).expect("conflicting root persists"),
        conflicting_target
    );
    let stderr = fixture.finish();
    assert!(
        stderr.contains("gateway.request_lease_release.failed"),
        "{stderr}"
    );
    assert!(stderr.contains("failure_class=\"retention\""), "{stderr}");
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
    let request_path = root.join("request-id");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat > '{}'\nprintf 'build-log-line\\n' >&2\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            request_path.display()
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
    let request_id = fs::read_to_string(&request_path).expect("helper records request ID");
    let helper_request: serde_json::Value =
        serde_json::from_str(&request_id).expect("helper request is JSON");
    let request_id = helper_request["request_id"]
        .as_str()
        .expect("helper request ID is a string");
    assert!(request_id.starts_with("request-"), "{request_id}");
    assert!(request_id.len() <= telchar::ipc::MAX_IPC_COMPONENT_BYTES);
    let persisted = telchar::persistence::read_build_request(fixture.database.url(), request_id)
        .expect("build request reads")
        .expect("build request exists before helper result");
    assert_eq!(persisted.request_id, request_id);
    assert_eq!(
        persisted.derivation_path,
        "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"
    );
    assert_eq!(persisted.system, "x86_64-linux");
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
fn accepted_builds_for_same_derivation_get_distinct_persisted_request_ids() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-request-identities-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let request_directory = root.join("requests");
    fs::create_dir(&request_directory).expect("request directory creates");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nrequest=$(mktemp '{}'/request-XXXXXX)\ncat > \"$request\"\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            request_directory.display()
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

    for _ in 0..2 {
        write_gate_3_build_derivation(&mut input, "x86_64-linux", 0);
        input.flush().expect("BuildDerivation request flushes");
        assert_eq!(read_integer(&mut output), STDERR_LAST);
        assert_eq!(read_integer(&mut output), 0, "Built status");
        assert_eq!(read_string(&mut output), "", "empty build error message");
        for _ in 0..7 {
            read_integer(&mut output);
        }
    }
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    let mut request_ids = fs::read_dir(&request_directory)
        .expect("request directory reads")
        .map(|entry| fs::read_to_string(entry.expect("request entry").path()))
        .collect::<Result<Vec<_>, _>>()
        .expect("helper requests read")
        .into_iter()
        .map(|request| {
            serde_json::from_str::<serde_json::Value>(&request).expect("helper request is JSON")
                ["request_id"]
                .as_str()
                .expect("helper request ID is a string")
                .to_owned()
        })
        .collect::<Vec<_>>();
    request_ids.sort();
    assert_eq!(request_ids.len(), 2);
    assert_ne!(request_ids[0], request_ids[1]);
    for request_id in &request_ids {
        assert!(request_id.starts_with("request-"), "{request_id}");
        assert!(request_id.len() <= telchar::ipc::MAX_IPC_COMPONENT_BYTES);
        assert_eq!(
            telchar::persistence::read_build_request(fixture.database.url(), request_id)
                .expect("build request reads")
                .expect("build request persists")
                .request_id,
            *request_id
        );
    }
    let mut database = fixture.database.connect();
    let mut leases = database
        .query(
            "SELECT lease_id, owner_id FROM store_leases WHERE purpose = 'derivation' ORDER BY owner_id",
            &[],
        )
        .expect("leases read")
        .into_iter()
        .map(|lease| (lease.get::<_, String>(0), lease.get::<_, String>(1)))
        .collect::<Vec<_>>();
    leases.sort_by(|left, right| left.1.cmp(&right.1));
    assert_eq!(leases.len(), 2);
    assert_ne!(leases[0].0, leases[1].0);
    for (lease_id, request_id) in leases {
        assert!(lease_id.starts_with("lease-"), "{lease_id}");
        assert_ne!(lease_id, request_id);
        assert!(lease_id.len() <= telchar::ipc::MAX_IPC_COMPONENT_BYTES);
    }
    let stderr = fixture.finish();
    assert!(
        stderr.contains("database.build_request.created"),
        "{stderr}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn derivation_lease_persistence_failure_retains_request_before_attachment_or_helper() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-derivation-lease-failure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let marker = root.join("helper-started");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf invoked > '{}'\nprintf 'unexpected-log\\n' >&2\n",
            marker.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [("TELCHAR_NIX_STORE_BUILD", helper.display().to_string())],
    );
    fixture
        .database
        .connect()
        .batch_execute(
            "CREATE FUNCTION reject_derivation_lease_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject derivation lease insert'; END $$; CREATE TRIGGER reject_derivation_lease_insert BEFORE INSERT ON store_leases FOR EACH ROW EXECUTE FUNCTION reject_derivation_lease_insert();",
        )
        .expect("failure trigger installs");
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
        "store lease state operation failed"
    );
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    assert!(!marker.exists(), "lease failure started the build helper");
    let mut database = fixture.database.connect();
    assert_eq!(
        database
            .query_one("SELECT count(*) FROM build_requests", &[])
            .expect("request count reads")
            .get::<_, i64>(0),
        1,
        "lease failure must retain the immutable request"
    );
    assert_eq!(
        database
            .query_one("SELECT count(*) FROM request_attachments", &[])
            .expect("attachment count reads")
            .get::<_, i64>(0),
        0,
        "lease failure must not create an attachment"
    );
    assert_eq!(
        database
            .query_one("SELECT count(*) FROM store_leases", &[])
            .expect("lease count reads")
            .get::<_, i64>(0),
        0,
        "failed lease transaction must not persist a lease"
    );
    let gc_roots = fixture.root.join("gc-roots");
    assert_eq!(
        fs::read_dir(&gc_roots)
            .expect("GC root directory reads")
            .count(),
        0,
        "derivation persistence failure retained a root"
    );
    let stderr = fixture.finish();
    assert!(stderr.contains("database.store_lease.failed"), "{stderr}");
    let retention_events = stderr
        .lines()
        .filter(|line| line.contains("event=\"gateway.store_retention\""))
        .collect::<Vec<_>>();
    assert_eq!(retention_events.len(), 2, "{stderr}");
    assert!(
        retention_events.iter().any(|line| {
            line.contains("operation=\"retain\"")
                && line.contains("purpose=\"derivation\"")
                && line.contains("path_count=1")
                && line.contains("result=\"succeeded\"")
        }) && retention_events.iter().any(|line| {
            line.contains("operation=\"rollback\"")
                && line.contains("purpose=\"derivation\"")
                && line.contains("path_count=1")
                && line.contains("result=\"succeeded\"")
        }),
        "{stderr}"
    );
    for event in retention_events {
        assert!(!event.contains("lease-"), "{event}");
        assert!(!event.contains("/nix/store/"), "{event}");
        assert!(!event.contains("gc-roots"), "{event}");
    }
    assert!(stderr.contains("operation=\"create\""), "{stderr}");
    assert!(stderr.contains("failure_class=\"query\""), "{stderr}");
    assert!(!stderr.contains("unexpected-log"), "{stderr}");
    assert!(
        !stderr.contains("reject derivation lease insert"),
        "{stderr}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn build_request_persistence_failure_rejects_before_helper_or_log_frame() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-build-request-failure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let marker = root.join("helper-started");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf invoked > '{}'\nprintf 'unexpected-log\\n' >&2\n",
            marker.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [("TELCHAR_NIX_STORE_BUILD", helper.display().to_string())],
    );
    fixture
        .database
        .connect()
        .batch_execute(
            "CREATE FUNCTION reject_build_request_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject insert'; END $$; CREATE TRIGGER reject_build_request_insert BEFORE INSERT ON build_requests FOR EACH ROW EXECUTE FUNCTION reject_build_request_insert();",
        )
        .expect("failure trigger installs");
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
        "build request state operation failed"
    );
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    assert!(
        !marker.exists(),
        "persistence failure started the build helper"
    );
    assert_eq!(
        fixture
            .database
            .connect()
            .query_one("SELECT count(*) FROM build_requests", &[])
            .expect("request count reads")
            .get::<_, i64>(0),
        0,
        "persistence failure created a build request"
    );
    let stderr = fixture.finish();
    assert!(stderr.contains("database.build_request.failed"), "{stderr}");
    assert!(stderr.contains("operation=\"create\""), "{stderr}");
    assert!(stderr.contains("failure_class=\"query\""), "{stderr}");
    assert!(!stderr.contains("unexpected-log"), "{stderr}");
    assert!(!stderr.contains("reject insert"), "{stderr}");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn request_attachment_failure_releases_roots_before_helper() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-attachment-failure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let marker = root.join("helper-started");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf invoked > '{}'\nprintf 'unexpected-log\\n' >&2\n",
            marker.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [("TELCHAR_NIX_STORE_BUILD", helper.display().to_string())],
    );
    fixture
        .database
        .connect()
        .batch_execute(
            "CREATE FUNCTION reject_attachment_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject insert'; END $$; CREATE TRIGGER reject_attachment_insert BEFORE INSERT ON request_attachments FOR EACH ROW EXECUTE FUNCTION reject_attachment_insert();",
        )
        .expect("attachment failure trigger installs");
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
        "request attachment state operation failed"
    );
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    assert!(!marker.exists(), "attachment failure started helper");
    let mut client = fixture.database.connect();
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM build_requests", &[])
            .expect("request count reads")
            .get::<_, i64>(0),
        1,
        "attachment failure discarded immutable request"
    );
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM request_attachments", &[])
            .expect("attachment count reads")
            .get::<_, i64>(0),
        0,
        "attachment failure persisted attachment"
    );
    assert_released_derivation_lease(&fixture.database, &request_id(&mut client));
    let stderr = fixture.finish();
    assert!(
        stderr.contains("database.request_attachment.failed"),
        "{stderr}"
    );
    assert!(stderr.contains("operation=\"attach\""), "{stderr}");
    assert!(stderr.contains("failure_class=\"query\""), "{stderr}");
    assert!(!stderr.contains("unexpected-log"), "{stderr}");
    assert!(!stderr.contains("reject insert"), "{stderr}");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn unread_frontend_backpressures_build_logs_and_disconnect_cleans_request() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-log-backpressure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let pid_path = root.join("pid");
    let started_path = root.join("started");
    let completed_path = root.join("completed");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > '{}'\ncat >/dev/null\nprintf started > '{}'\nprintf 'telchar-hostile-log-secret' >&2\nhead -c 67108864 /dev/zero >&2\nprintf completed > '{}'\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            pid_path.display(),
            started_path.display(),
            completed_path.display()
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
    while !started_path.exists() {
        assert!(
            Instant::now() < deadline,
            "helper did not start log production"
        );
        thread::sleep(Duration::from_millis(5));
    }

    let blocked_deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < blocked_deadline {
        assert!(
            !completed_path.exists(),
            "helper completed 64 MiB log production while frontend output was unread"
        );
        assert!(
            child.try_wait().expect("frontend status reads").is_none(),
            "frontend exited instead of applying backpressure"
        );
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
            "backpressured helper remains alive after disconnect"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !completed_path.exists(),
        "disconnect allowed blocked helper to complete log production"
    );

    let mut client = fixture.database.connect();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = client
            .query_one("SELECT state FROM request_attachments", &[])
            .expect("attachment state reads")
            .get::<_, String>(0);
        if state == "detached" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "backpressured disconnect left attachment attached"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_released_derivation_lease(&fixture.database, &request_id(&mut client));
    let mut frontend_stderr = String::new();
    fixture
        .frontend
        .stderr
        .take()
        .expect("frontend stderr")
        .read_to_string(&mut frontend_stderr)
        .expect("frontend stderr reads");
    let daemon_output = fixture.daemon.wait_with_output().expect("daemon exits");
    assert!(
        !frontend_stderr.contains("telchar-hostile-log-secret"),
        "{frontend_stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&daemon_output.stderr).contains("telchar-hostile-log-secret"),
        "{:?}",
        daemon_output.stderr
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn detached_frontend_allows_failed_helper_to_finish_without_dead_transport_write() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-detached-failure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let started = root.join("started");
    let complete = root.join("complete");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nexit 1\n",
            started.display(),
            complete.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store_default_disconnect_policy(
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
    while !started.exists() {
        assert!(Instant::now() < deadline, "helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    child.kill().expect("frontend terminates");
    child.wait().expect("frontend reaps");
    drop(input);
    drop(output);

    fs::write(&complete, b"complete").expect("helper completion releases");
    let mut database = fixture.database.connect();
    let request_id = request_id(&mut database);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = database
            .query_one("SELECT state FROM request_attachments", &[])
            .expect("attachment state reads")
            .get::<_, String>(0);
        if state == "detached" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "requester disconnect left attachment attached"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_released_derivation_lease(&fixture.database, &request_id);
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.build_derivation.failed"),
        "{stderr}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn detached_frontend_suppresses_output_validation_failure() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-detached-invalid-output-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let build_helper = root.join("build-helper");
    let started = root.join("started");
    let complete = root.join("complete");
    fs::write(
        &build_helper,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            started.display(),
            complete.display()
        ),
    )
    .expect("build helper writes");
    fs::set_permissions(&build_helper, fs::Permissions::from_mode(0o700))
        .expect("build helper executable");
    let nar_path = root.join("output.nar");
    fs::write(&nar_path, regular_nar(b"detached-output-metadata-secret"))
        .expect("output NAR writes");
    let export_helper = root.join("export-helper");
    fs::write(
        &export_helper,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\ncat '{}'\n",
            nar_path.display()
        ),
    )
    .expect("export helper writes");
    fs::set_permissions(&export_helper, fs::Permissions::from_mode(0o700))
        .expect("export helper executable");
    let nix = root.join("nix");
    fs::write(
        &nix,
        "#!/bin/sh\nset -eu\nprintf '{\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\":{\"narHash\":\"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\",\"narSize\":136,\"references\":[],\"deriver\":null,\"ca\":null}}\\n'\n",
    )
    .expect("Nix query helper writes");
    fs::set_permissions(&nix, fs::Permissions::from_mode(0o700)).expect("Nix helper executable");
    let mut fixture = FrontendFixture::spawn_with_store_default_disconnect_policy(
        None,
        "unix:///fixed-gateway.sock",
        [
            (
                "TELCHAR_NIX_STORE_BUILD",
                build_helper.display().to_string(),
            ),
            (
                "TELCHAR_NIX_STORE_EXPORT",
                export_helper.display().to_string(),
            ),
            ("TELCHAR_NIX", nix.display().to_string()),
        ],
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);
    write_gate_3_build_derivation(&mut input, "x86_64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");

    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        assert!(Instant::now() < deadline, "helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    child.kill().expect("frontend terminates");
    child.wait().expect("frontend reaps");
    drop(input);
    let mut detached_output = Vec::new();
    output
        .read_to_end(&mut detached_output)
        .expect("detached frontend output closes");
    assert!(detached_output.is_empty(), "dead requester received output");

    fs::write(&complete, b"complete").expect("helper completion releases");
    let mut database = fixture.database.connect();
    let request_id = request_id(&mut database);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = database
            .query_one("SELECT state FROM request_attachments", &[])
            .expect("attachment state reads")
            .get::<_, String>(0);
        if state == "detached" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "requester disconnect left attachment attached"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_released_derivation_lease(&fixture.database, &request_id);
    assert_eq!(
        database
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = $1 AND state = 'active'",
                &[&request_id],
            )
            .expect("active lease count reads")
            .get::<_, i64>(0),
        0,
        "invalid detached output left active leases"
    );
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.build_derivation.output_validation_failed"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("detached-output-metadata-secret"),
        "{stderr}"
    );
    assert!(!stderr.contains(&request_id), "{stderr}");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn detached_frontend_finishes_valid_output_and_retains_output_resources() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-detached-success-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let pid_path = root.join("pid");
    let started = root.join("started");
    let complete = root.join("complete");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '%s\\n' \"$$\" > '{}'\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf 'detached-build-log\\n' >&2\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            pid_path.display(),
            started.display(),
            complete.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store_default_disconnect_policy(
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
    while !started.exists() {
        assert!(Instant::now() < deadline, "helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    let pid = fs::read_to_string(&pid_path).expect("helper recorded PID");
    child.kill().expect("frontend terminates");
    child.wait().expect("frontend reaps");
    drop(input);
    let mut detached_output = Vec::new();
    output
        .read_to_end(&mut detached_output)
        .expect("detached frontend output closes");
    assert!(detached_output.is_empty(), "dead requester received output");
    assert!(
        Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("process liveness query runs")
            .success(),
        "detach-and-finish cancelled helper"
    );

    fs::write(&complete, b"complete").expect("helper completion releases");
    let mut database = fixture.database.connect();
    let request_id = request_id(&mut database);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = database
            .query_one("SELECT state FROM request_attachments", &[])
            .expect("attachment state reads")
            .get::<_, String>(0);
        if state == "detached" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "requester disconnect left attachment attached"
        );
        thread::sleep(Duration::from_millis(5));
    }
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
        assert!(Instant::now() < deadline, "completed helper remains alive");
        thread::sleep(Duration::from_millis(5));
    }
    assert_released_derivation_lease(&fixture.database, &request_id);
    let output_leases = database
        .query(
            "SELECT lease_id, store_path, state FROM store_leases WHERE owner_id = $1 AND purpose = 'output' ORDER BY lease_id",
            &[&request_id],
        )
        .expect("output leases read");
    assert_eq!(output_leases.len(), 1, "detached output lease count");
    let output_lease_id = output_leases[0].get::<_, String>(0);
    assert_eq!(
        output_leases[0].get::<_, String>(1),
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"
    );
    assert_eq!(output_leases[0].get::<_, String>(2), "active");
    assert_eq!(
        fs::read_link(fixture.root.join("gc-roots").join(output_lease_id))
            .expect("output root reads"),
        PathBuf::from("/nix/store/11111111111111111111111111111111-telchar-gate-3-contract")
    );
    assert_eq!(
        database
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = $1 AND purpose IN ('derivation', 'input') AND state = 'active'",
                &[&request_id],
            )
            .expect("active request lease count reads")
            .get::<_, i64>(0),
        0,
        "detached completion retained request leases"
    );
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.build_derivation.completed"),
        "{stderr}"
    );
    assert!(!stderr.contains("detached-build-log"), "{stderr}");
    assert!(!stderr.contains(&request_id), "{stderr}");
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
    let mut client = fixture.database.connect();
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM build_requests", &[])
            .expect("request count reads")
            .get::<_, i64>(0),
        1,
        "requester disconnect discarded the immutable build request"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = client
            .query_one("SELECT state FROM request_attachments", &[])
            .expect("attachment state reads")
            .get::<_, String>(0);
        if state == "detached" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "requester disconnect left attachment attached"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_released_derivation_lease(&fixture.database, &request_id(&mut client));
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.build_derivation.failed"),
        "{stderr}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn valid_build_derivation_is_consumed_before_execution_unavailable_error() {
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        std::iter::empty::<(&str, String)>(),
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
    assert_eq!(read_string(&mut output), "BuildDerivation execution failed");
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    let mut client = fixture.database.connect();
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM build_requests", &[])
            .expect("request count reads")
            .get::<_, i64>(0),
        1,
        "execution failure discarded the immutable build request"
    );
    assert_eq!(
        client
            .query_one("SELECT state FROM request_attachments", &[])
            .expect("attachment state reads")
            .get::<_, String>(0),
        "detached",
        "execution failure left attachment attached"
    );
    assert_released_derivation_lease(&fixture.database, &request_id(&mut client));
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.build_derivation.admitted"),
        "{stderr}"
    );
    assert!(
        stderr.contains("worker.build_derivation.failed"),
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
    assert_eq!(
        fixture
            .database
            .connect()
            .query_one("SELECT count(*) FROM build_requests", &[])
            .expect("request count reads")
            .get::<_, i64>(0),
        0,
        "rejected system persisted a build request"
    );
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
    database: PostgresFixture,
}

impl FrontendFixture {
    fn spawn(worker_timeout_ms: Option<u64>) -> Self {
        Self::spawn_configured(
            worker_timeout_ms,
            None,
            std::iter::empty::<(&str, String)>(),
            Some("cancel-running"),
        )
    }

    fn spawn_with_store(
        worker_timeout_ms: Option<u64>,
        store_uri: &str,
        environment: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        Self::spawn_with_store_policy(
            worker_timeout_ms,
            store_uri,
            environment,
            Some("cancel-running"),
        )
    }

    fn spawn_with_store_default_disconnect_policy(
        worker_timeout_ms: Option<u64>,
        store_uri: &str,
        environment: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        Self::spawn_with_store_policy(worker_timeout_ms, store_uri, environment, None)
    }

    fn spawn_with_store_policy(
        worker_timeout_ms: Option<u64>,
        store_uri: &str,
        environment: impl IntoIterator<Item = (&'static str, String)>,
        running_disconnect_policy: Option<&str>,
    ) -> Self {
        let environment = environment.into_iter().collect::<Vec<_>>();
        let has_export = environment
            .iter()
            .any(|(name, _)| *name == "TELCHAR_NIX_STORE_EXPORT");
        let has_build = environment
            .iter()
            .any(|(name, _)| *name == "TELCHAR_NIX_STORE_BUILD");
        if has_export || !has_build {
            Self::spawn_configured(
                worker_timeout_ms,
                Some(store_uri),
                environment,
                running_disconnect_policy,
            )
        } else {
            let root = std::env::temp_dir().join(format!(
                "telchar-operation-export-{}-{}",
                std::process::id(),
                FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("export fixture root creates");
            let nar_path = root.join("output.nar");
            fs::write(&nar_path, regular_nar(b"telchar-classic-fixture"))
                .expect("export fixture NAR writes");
            let export_helper = root.join("export-helper");
            fs::write(
                &export_helper,
                format!(
                    "#!/bin/sh\nset -eu\ncat >/dev/null\ncat '{}'\n",
                    nar_path.display()
                ),
            )
            .expect("export helper writes");
            fs::set_permissions(&export_helper, fs::Permissions::from_mode(0o700))
                .expect("export helper executable");
            let nix = root.join("nix");
            fs::write(
                &nix,
                "#!/bin/sh\nset -eu\nprintf '{\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\":{\"narHash\":\"sha256-bCvi8SoWhgXry6N4IobDjq8/XXh7eouySlQNImf/aOE=\",\"narSize\":136,\"references\":[],\"deriver\":null,\"ca\":null}}\\n'\n",
            )
            .expect("Nix query helper writes");
            fs::set_permissions(&nix, fs::Permissions::from_mode(0o700))
                .expect("Nix helper executable");
            let mut environment = environment;
            environment.push((
                "TELCHAR_NIX_STORE_EXPORT",
                export_helper.display().to_string(),
            ));
            environment.push(("TELCHAR_NIX", nix.display().to_string()));
            Self::spawn_configured(
                worker_timeout_ms,
                Some(store_uri),
                environment,
                running_disconnect_policy,
            )
        }
    }

    fn spawn_configured(
        worker_timeout_ms: Option<u64>,
        store_uri: Option<&str>,
        environment: impl IntoIterator<Item = (&'static str, String)>,
        running_disconnect_policy: Option<&str>,
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
        let gc_roots = store_uri.map(|_| root.join("gc-roots"));
        if let Some(gc_roots) = &gc_roots {
            fs::create_dir(gc_roots).expect("GC root directory creates");
            fs::set_permissions(gc_roots, fs::Permissions::from_mode(0o700))
                .expect("GC root directory permissions set");
        }
        let configured_store_uri = store_uri.map(str::to_owned);
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
            .env_remove("TELCHAR_RUNNING_DISCONNECT_POLICY")
            .env("TELCHAR_SUPPORTED_FEATURES", "")
            .env_remove("TELCHAR_NIX_STORE_BUILD")
            .env_remove("TELCHAR_TEST_BUILD_HELPER")
            .env_remove("TELCHAR_NIX_STORE_EXPORT")
            .env_remove("TELCHAR_TEST_EXPORT_HELPER")
            .env_remove("TELCHAR_NIX_STORE_PROMOTE")
            .env_remove("TELCHAR_GATEWAY_STORE_URI")
            .env_remove("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY");
        if let Some(gc_roots) = gc_roots {
            daemon_command.env("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY", gc_roots);
        }
        if let Some(running_disconnect_policy) = running_disconnect_policy {
            daemon_command.env(
                "TELCHAR_RUNNING_DISCONNECT_POLICY",
                running_disconnect_policy,
            );
        }
        if let Some(timeout) = worker_timeout_ms {
            daemon_command.env("TELCHAR_WORKER_IDLE_TIMEOUT_MS", timeout.to_string());
        }
        if let Some(store_uri) = configured_store_uri {
            daemon_command.env("TELCHAR_GATEWAY_STORE_URI", store_uri);
        }
        let environment = environment
            .into_iter()
            .map(|(name, value)| match name {
                "TELCHAR_NIX_STORE_BUILD" => ("TELCHAR_TEST_BUILD_HELPER", value),
                "TELCHAR_NIX_STORE_EXPORT" => ("TELCHAR_TEST_EXPORT_HELPER", value),
                _ => (name, value),
            })
            .collect::<Vec<_>>();
        if store_uri == Some("unix:///fixed-gateway.sock") {
            daemon_command.env("TELCHAR_TEST_STORE_RETENTION", "filesystem-only");
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
            database,
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

fn request_id(database: &mut postgres::Client) -> String {
    database
        .query_one("SELECT request_id FROM build_requests", &[])
        .expect("request ID reads")
        .get(0)
}

fn assert_active_derivation_lease(database: &PostgresFixture, request_id: &str) {
    let lease = database
        .connect()
        .query_one(
            "SELECT owner_kind, owner_id, store_path, purpose, state FROM store_leases WHERE owner_id = $1",
            &[&request_id],
        )
        .expect("active derivation lease reads");
    assert_eq!(lease.get::<_, String>(0), "request");
    assert_eq!(lease.get::<_, String>(1), request_id);
    assert_eq!(
        lease.get::<_, String>(2),
        "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"
    );
    assert_eq!(lease.get::<_, String>(3), "derivation");
    assert_eq!(lease.get::<_, String>(4), "active");
}

fn assert_released_derivation_lease(database: &PostgresFixture, request_id: &str) {
    let lease = database
        .connect()
        .query_one(
            "SELECT state, released_at FROM store_leases WHERE owner_id = $1 AND purpose = 'derivation'",
            &[&request_id],
        )
        .expect("released derivation lease reads");
    assert_eq!(lease.get::<_, String>(0), "released");
    assert!(lease.get::<_, Option<SystemTime>>(1).is_some());
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

fn spawn_closure_daemon(
    socket: &std::path::Path,
    expect_output_registration: bool,
) -> thread::JoinHandle<()> {
    let listener = UnixListener::bind(socket).expect("closure daemon socket binds");
    thread::spawn(move || {
        // First connection protects the derivation before closure discovery.
        // Worker op 11 is AddTempRoot; op 12 registers Telchar's indirect GC root.
        let (mut stream, _) = listener.accept().expect("closure daemon accepts");
        assert_eq!(read_integer(&mut stream), CLIENT_WORKER_MAGIC);
        assert_eq!(read_integer(&mut stream), LATEST_WORKER_VERSION.to_wire());
        write_integer(&mut stream, SERVER_WORKER_MAGIC);
        write_integer(&mut stream, LATEST_WORKER_VERSION.to_wire());
        stream.flush().expect("closure greeting flushes");
        assert_eq!(read_integer(&mut stream), 0);
        write_integer(&mut stream, 0);
        stream.flush().expect("closure features flush");
        assert_eq!(read_integer(&mut stream), 0);
        assert_eq!(read_integer(&mut stream), 0);
        write_string(&mut stream, b"2.34.8");
        write_integer(&mut stream, 1);
        write_integer(&mut stream, STDERR_LAST);
        stream.flush().expect("closure handshake flushes");

        assert_eq!(read_integer(&mut stream), 11);
        assert_eq!(
            read_string(&mut stream),
            "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"
        );
        write_integer(&mut stream, STDERR_LAST);
        write_integer(&mut stream, 1);
        stream.flush().expect("temporary root response flushes");
        assert_eq!(read_integer(&mut stream), 12);
        let indirect_root = read_string(&mut stream);
        assert!(indirect_root.contains("gc-roots"));
        write_integer(&mut stream, STDERR_LAST);
        write_integer(&mut stream, 1);
        stream.flush().expect("indirect root response flushes");
        // Closure traversal owns a separate daemon connection. Worker op 26 is
        // QueryPathInfo. The response is: present, deriver, NAR hash, references,
        // registration time, NAR size, ultimate, signatures, content address.
        let (mut stream, _) = listener.accept().expect("closure query connection accepts");
        assert_eq!(read_integer(&mut stream), CLIENT_WORKER_MAGIC);
        assert_eq!(read_integer(&mut stream), LATEST_WORKER_VERSION.to_wire());
        write_integer(&mut stream, SERVER_WORKER_MAGIC);
        write_integer(&mut stream, LATEST_WORKER_VERSION.to_wire());
        stream.flush().expect("closure query greeting flushes");
        assert_eq!(read_integer(&mut stream), 0);
        write_integer(&mut stream, 0);
        stream.flush().expect("closure query features flush");
        assert_eq!(read_integer(&mut stream), 0);
        assert_eq!(read_integer(&mut stream), 0);
        write_string(&mut stream, b"2.34.8");
        write_integer(&mut stream, 1);
        write_integer(&mut stream, STDERR_LAST);
        stream.flush().expect("closure query handshake flushes");
        assert_eq!(read_integer(&mut stream), 26);
        assert_eq!(
            read_string(&mut stream),
            "/nix/store/22222222222222222222222222222222-telchar-input"
        );
        write_integer(&mut stream, STDERR_LAST); // no daemon diagnostics
        write_integer(&mut stream, 1); // path metadata is present
        write_string(&mut stream, b""); // no deriver
        write_string(
            &mut stream,
            b"6c2be2f12a168605ebcba3782286c38eaf3f5d787b7a8bb24a540d2267ff68e1",
        );
        write_integer(&mut stream, 0); // no references: closure leaf
        write_integer(&mut stream, 0); // registration time
        write_integer(&mut stream, 0); // NAR size is irrelevant to traversal
        write_integer(&mut stream, 0); // not ultimate
        write_integer(&mut stream, 0); // no signatures
        write_string(&mut stream, b""); // no content address
        stream.flush().expect("closure response flushes");

        handle_root_registration(&listener); // input root
        if expect_output_registration {
            handle_root_registration(&listener); // verified output root
        }
    })
}

fn handle_root_registration(listener: &UnixListener) {
    // Input retention opens one connection per retained path. It sends AddTempRoot
    // (op 11), creates the symlink locally, then sends AddIndirectRoot (op 12).
    let (mut stream, _) = listener.accept().expect("root registration accepts");
    assert_eq!(read_integer(&mut stream), CLIENT_WORKER_MAGIC);
    assert_eq!(read_integer(&mut stream), LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut stream, SERVER_WORKER_MAGIC);
    write_integer(&mut stream, LATEST_WORKER_VERSION.to_wire());
    stream.flush().expect("root registration greeting flushes");
    assert_eq!(read_integer(&mut stream), 0);
    write_integer(&mut stream, 0);
    stream.flush().expect("root registration features flush");
    assert_eq!(read_integer(&mut stream), 0);
    assert_eq!(read_integer(&mut stream), 0);
    write_string(&mut stream, b"2.34.8");
    write_integer(&mut stream, 1);
    write_integer(&mut stream, STDERR_LAST);
    stream.flush().expect("root registration handshake flushes");
    assert_eq!(read_integer(&mut stream), 11);
    assert!(read_string(&mut stream).starts_with("/nix/store/"));
    write_integer(&mut stream, STDERR_LAST);
    write_integer(&mut stream, 1);
    stream
        .flush()
        .expect("root registration temporary response flushes");
    assert_eq!(read_integer(&mut stream), 12);
    assert!(read_string(&mut stream).contains("gc-roots"));
    write_integer(&mut stream, STDERR_LAST);
    write_integer(&mut stream, 1);
    stream
        .flush()
        .expect("root registration indirect response flushes");
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

fn write_input_build_derivation(output: &mut impl Write, system: &str, mode: u64) {
    let source = b"/nix/store/22222222222222222222222222222222-telchar-input";
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
    write_integer(output, 1);
    write_string(output, source);
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

fn regular_nar(contents: &[u8]) -> Vec<u8> {
    let mut nar = Vec::new();
    for value in [
        b"nix-archive-1".as_slice(),
        b"(".as_slice(),
        b"type".as_slice(),
        b"regular".as_slice(),
        b"contents".as_slice(),
        contents,
        b")".as_slice(),
    ] {
        write_string(&mut nar, value);
    }
    nar
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
