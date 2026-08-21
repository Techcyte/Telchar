//! Tests store query and AddMultipleToStore transfer dispatch.

use super::*;

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
fn query_missing_reports_uncached_derivation_as_buildable() {
    let name = format!("telchar-query-missing-{}", std::process::id());
    let expression = format!(
        "derivation {{ name = \"{name}\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf {name} > \\\"$out\\\"\" ]; }}"
    );
    let derivation = Command::new("nix-instantiate")
        .args(["--expr", &expression])
        .output()
        .unwrap_or_else(|error| panic!("host-store fixture derivation failed to run: {error}"));
    assert!(
        derivation.status.success(),
        "nix-instantiate failed: {}",
        String::from_utf8_lossy(&derivation.stderr)
    );
    let derivation = std::str::from_utf8(&derivation.stdout)
        .unwrap_or_else(|error| panic!("derivation path is not UTF-8: {error}"))
        .trim();
    assert!(derivation.starts_with("/nix/store/") && !derivation.contains('\n'));
    let derivation = PathBuf::from(derivation);
    let mut frontend =
        FrontendFixture::spawn_with_store(None, "unix:///nix/var/nix/daemon-socket/socket", []);
    let child = &mut frontend.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 40);
    write_integer(&mut input, 1);
    let target = format!("{}!*", derivation.display());
    write_string(&mut input, target.as_bytes());
    input.flush().expect("QueryMissing request flushes");

    let frame = read_integer(&mut output);
    if frame != STDERR_LAST {
        drop(input);
        let _ = child.wait();
        panic!(
            "unexpected QueryMissing frame {frame:#x}: {}",
            frontend.finish()
        );
    }
    assert_eq!(read_integer(&mut output), 1, "derivation needs building");
    assert_eq!(read_string(&mut output), derivation.to_string_lossy());
    assert_eq!(read_integer(&mut output), 0, "nothing will substitute");
    assert_eq!(read_integer(&mut output), 0, "target is known");
    assert_eq!(read_integer(&mut output), 0, "no download bytes");
    assert_eq!(read_integer(&mut output), 0, "no substitute NAR bytes");
    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    let stderr = frontend.finish();
    assert!(
        stderr.contains("worker.query_missing.completed"),
        "missing completion telemetry: {stderr}"
    );
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
            ("TELCHAR_TEST_PROMOTE_HELPER", helper.display().to_string()),
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
