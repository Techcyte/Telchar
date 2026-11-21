//! Tests shared execution coalescing and leader/follower disconnect ownership.

use super::*;

#[test]
fn real_output_fan_in_validates_one_nar() {
    let root = tempfile::tempdir().unwrap();
    let started = root.path().join("started");
    let release = root.path().join("release");
    let sleep = Command::new("sh")
        .args(["-c", "command -v sleep"])
        .output()
        .unwrap();
    assert!(sleep.status.success());
    let sleep = String::from_utf8(sleep.stdout).unwrap();
    let expression = format!(
        r#"derivation {{ name = "telchar-validation-fan-in"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "echo started > {}; while [ ! -e {} ]; do {} 0.01; done; printf fan-in-output > $out" ]; }}"#,
        started.display(),
        release.display(),
        sleep.trim()
    );
    let instantiated = Command::new("nix-instantiate")
        .args(["--store", "local", "--expr", &expression])
        .output()
        .unwrap();
    assert!(instantiated.status.success(), "{instantiated:?}");
    let drv = String::from_utf8(instantiated.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let kind = telchar::backend::BackendKind::Local;
    let build = telchar::build::BuildRequest::from_stored_derivation(
        drv.as_bytes(),
        &fs::read(&drv).unwrap(),
        &[
            telchar::backend::BackendTarget::new("local", kind, "x86_64-linux", [] as [&str; 0])
                .unwrap(),
        ],
    )
    .unwrap();
    let mut fixture =
        FrontendFixture::spawn_multi_with_store("unix:///nix/var/nix/daemon-socket/socket", []);
    let mut follower = fixture.spawn_frontend();
    let mut leader_input = fixture.frontend.stdin.take().unwrap();
    let mut leader_output = fixture.frontend.stdout.take().unwrap();
    let mut follower_input = follower.stdin.take().unwrap();
    let mut follower_output = follower.stdout.take().unwrap();
    complete_handshake(&mut leader_input, &mut leader_output);
    complete_handshake(&mut follower_input, &mut follower_output);
    super::build_failure::send_failure_build(&mut leader_input, &build, "", false);
    wait_for_file_for(
        &started,
        Duration::from_secs(10),
        "real leader did not start",
    );
    super::build_failure::send_failure_build(&mut follower_input, &build, "", false);
    assert_eq!(
        read_integer(&mut follower_output),
        nix_worker_protocol::STDERR_NEXT
    );
    assert_eq!(
        read_string(&mut follower_output),
        "identical build already in progress\n"
    );
    fs::write(&release, "release").unwrap();
    let leader = thread::spawn(move || read_build_success(leader_output));
    let following = thread::spawn(move || read_build_success(follower_output));
    leader.join().unwrap().unwrap();
    following.join().unwrap().unwrap();
    drop(leader_input);
    drop(follower_input);
    assert!(follower.wait().unwrap().success());
    assert!(fixture.frontend.wait().unwrap().success());
    let lease_owners: i64 = fixture.database.connect().query_one(
        "SELECT count(DISTINCT owner_id) FROM store_leases WHERE purpose = 'output' AND state = 'active'", &[]
    ).unwrap().get(0);
    assert_eq!(
        lease_owners, 2,
        "each coalesced requester retains retrieval authority"
    );
    let path = &build.expected_outputs()[0].1;
    let endpoint = telchar::store::daemon::GatewayStoreEndpoint::parse(
        "unix:///nix/var/nix/daemon-socket/socket",
    )
    .unwrap();
    let info = telchar::store::daemon::GatewayStoreConnection::connect(&endpoint)
        .unwrap()
        .query_path_info(path)
        .unwrap()
        .unwrap();
    let mut later = fixture.spawn_frontend();
    let mut later_input = later.stdin.take().unwrap();
    let mut later_output = later.stdout.take().unwrap();
    complete_handshake(&mut later_input, &mut later_output);
    super::build_failure::send_failure_build(&mut later_input, &build, "", false);
    loop {
        let frame = read_integer(&mut later_output);
        if frame == STDERR_LAST {
            break;
        }
        assert_eq!(frame, nix_worker_protocol::STDERR_NEXT);
        read_string(&mut later_output);
    }
    assert_eq!(
        read_integer(&mut later_output),
        2,
        "independent reuse is AlreadyValid"
    );
    assert!(read_string(&mut later_output).is_empty());
    for _ in 0..6 {
        read_integer(&mut later_output);
    }
    let output_count = read_integer(&mut later_output);
    assert_eq!(output_count, 1);
    for _ in 0..output_count {
        read_string(&mut later_output);
        read_string(&mut later_output);
    }
    drop(later_input);
    assert!(later.wait().unwrap().success());
    assert!(
        std::env::var_os("TELCHAR_STORE_ROOT").is_some(),
        "output corruption requires disposable canonical store"
    );
    let output_path = std::str::from_utf8(path).unwrap();
    fs::set_permissions(output_path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(output_path, b"bad-in-output").unwrap();
    let mut corrupted = fixture.spawn_frontend();
    let mut corrupted_input = corrupted.stdin.take().unwrap();
    let mut corrupted_output = corrupted.stdout.take().unwrap();
    complete_handshake(&mut corrupted_input, &mut corrupted_output);
    super::build_failure::send_failure_build(&mut corrupted_input, &build, "", false);
    while let Ok(frame) = try_read_integer(&mut corrupted_output) {
        if frame == nix_worker_protocol::STDERR_NEXT {
            read_string(&mut corrupted_output);
            continue;
        }
        assert_eq!(
            frame,
            nix_worker_protocol::STDERR_ERROR,
            "corrupted output must not return success"
        );
        break;
    }
    drop(corrupted_input);
    drop(corrupted_output);
    corrupted.wait().unwrap();
    let diagnostics = fixture.finish();
    assert!(
        diagnostics.contains("exported NAR hash mismatch")
            || diagnostics.contains("exported NAR size mismatch"),
        "{diagnostics}"
    );
    let validations = diagnostics
        .lines()
        .filter(|line| line.contains("event=\"gateway.output.validated\""))
        .collect::<Vec<_>>();
    assert_eq!(
        validations.len(),
        2,
        "one shared validation and one independent reuse validation: {diagnostics}"
    );
    for validation in validations {
        let bytes = validation
            .split("nar_bytes=")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert_eq!(bytes, info.nar_size());
    }
}

#[test]
fn concurrent_identical_frontends_share_one_build_execution() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-shared-build-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let invocation_count = root.join("invocations");
    let started = root.join("started");
    let complete = root.join("complete");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf x >> '{}'\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract\"]]}}\\n'\n",
            invocation_count.display(),
            started.display(),
            complete.display(),
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_multi_with_store(
        "unix:///fixed-gateway.sock",
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
    );
    let mut leader_input = fixture.frontend.stdin.take().expect("leader input");
    let mut leader_output = fixture.frontend.stdout.take().expect("leader output");
    complete_handshake(&mut leader_input, &mut leader_output);
    let mut follower = fixture.spawn_frontend();
    let mut follower_input = follower.stdin.take().expect("follower input");
    let mut follower_output = follower.stdout.take().expect("follower output");
    complete_handshake(&mut follower_input, &mut follower_output);

    write_build_derivation_request(&mut leader_input, "x86_64-linux", 0);
    leader_input.flush().expect("leader request flushes");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        assert!(Instant::now() < deadline, "leader helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    write_build_derivation_request(&mut follower_input, "x86_64-linux", 0);
    follower_input.flush().expect("follower request flushes");

    assert_eq!(
        read_integer(&mut follower_output),
        nix_worker_protocol::STDERR_NEXT
    );
    assert_eq!(
        read_string(&mut follower_output),
        "identical build already in progress\n"
    );
    assert_eq!(
        fs::read_to_string(&invocation_count).expect("invocation count reads"),
        "x"
    );

    fs::write(&complete, b"complete").expect("helper completion releases");
    let leader_response = thread::spawn(move || read_build_success(leader_output));
    let follower_response = thread::spawn(move || read_build_success(follower_output));
    leader_response
        .join()
        .expect("leader response reader joins")
        .expect("leader build succeeds");
    follower_response
        .join()
        .expect("follower response reader joins")
        .expect("follower build succeeds");
    assert_eq!(
        fs::read_to_string(&invocation_count).expect("invocation count reads"),
        "x",
        "duplicate helper invocation detected"
    );
    drop(leader_input);
    drop(follower_input);
    let follower_status = follower.wait().expect("follower exits");
    let leader_status = fixture.frontend.wait().expect("leader exits");
    let mut follower_stderr = String::new();
    follower
        .stderr
        .take()
        .expect("follower stderr")
        .read_to_string(&mut follower_stderr)
        .expect("follower stderr reads");
    assert!(follower_status.success(), "{follower_stderr}");
    assert!(leader_status.success());
    let shared_build = telchar::persistence::read_shared_build(
        fixture.database.url(),
        "/nix/store/00000000000000000000000000000000-telchar-build-derivation-contract.drv",
    )
    .expect("shared build reads")
    .expect("shared build exists");
    assert_eq!(
        shared_build.state,
        telchar::persistence::SharedBuildState::Succeeded
    );
    let attempt = telchar::persistence::read_shared_build_attempt(
        fixture.database.url(),
        "/nix/store/00000000000000000000000000000000-telchar-build-derivation-contract.drv",
    )
    .expect("shared build attempt reads")
    .expect("shared build attempt exists");
    assert_eq!(attempt.ordinal, 1);
    assert_eq!(
        attempt.state,
        telchar::persistence::SharedBuildAttemptState::Succeeded
    );
    let outcome = telchar::persistence::read_shared_build_attempt_outcome(
        fixture.database.url(),
        &attempt.attempt_id,
    )
    .expect("shared build attempt outcome reads")
    .expect("shared build attempt outcome exists");
    assert_eq!(outcome.classification, "succeeded");
    assert_eq!(
        fs::read_to_string(&invocation_count).expect("invocation count reads"),
        "x"
    );
    let diagnostics = fixture.finish();
    let validations = diagnostics
        .lines()
        .filter(|line| {
            line.contains("event=\"server.phase.started\"")
                && line.contains("phase=\"validate-outputs\"")
        })
        .count();
    assert_eq!(
        validations, 1,
        "coalesced clients repeated output validation: {diagnostics}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn disconnected_follower_does_not_cancel_shared_build() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-shared-build-follower-disconnect-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let invocation_count = root.join("invocations");
    let started = root.join("started");
    let complete = root.join("complete");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf x >> '{}'\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract\"]]}}\\n'\n",
            invocation_count.display(),
            started.display(),
            complete.display(),
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_multi_with_store(
        "unix:///fixed-gateway.sock",
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
    );
    let mut leader_input = fixture.frontend.stdin.take().expect("leader input");
    let mut leader_output = fixture.frontend.stdout.take().expect("leader output");
    complete_handshake(&mut leader_input, &mut leader_output);

    let mut follower = fixture.spawn_frontend();
    let mut follower_input = follower.stdin.take().expect("follower input");
    let mut follower_output = follower.stdout.take().expect("follower output");
    complete_handshake(&mut follower_input, &mut follower_output);

    write_build_derivation_request(&mut leader_input, "x86_64-linux", 0);
    leader_input.flush().expect("leader request flushes");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        assert!(Instant::now() < deadline, "leader helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    write_build_derivation_request(&mut follower_input, "x86_64-linux", 0);
    follower_input.flush().expect("follower request flushes");
    assert_eq!(
        read_integer(&mut follower_output),
        nix_worker_protocol::STDERR_NEXT
    );
    assert_eq!(
        read_string(&mut follower_output),
        "identical build already in progress\n"
    );

    follower.kill().expect("follower terminates");
    follower.wait().expect("follower reaps");
    drop(follower_input);
    drop(follower_output);
    fs::write(&complete, b"complete").expect("helper completion releases");
    fixture.frontend.kill().expect("leader terminates");
    fixture.frontend.wait().expect("leader reaps");
    drop(leader_input);
    drop(leader_output);
    assert_eq!(
        fs::read_to_string(&invocation_count).expect("invocation count reads"),
        "x"
    );
    fixture.finish();
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn disconnected_leader_does_not_cancel_shared_build() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-shared-build-leader-disconnect-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let invocation_count = root.join("invocations");
    let started = root.join("started");
    let complete = root.join("complete");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf x >> '{}'\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract\"]]}}\\n'\n",
            invocation_count.display(),
            started.display(),
            complete.display(),
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_multi_with_store(
        "unix:///fixed-gateway.sock",
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
    );
    let mut leader_input = fixture.frontend.stdin.take().expect("leader input");
    let mut leader_output = fixture.frontend.stdout.take().expect("leader output");
    complete_handshake(&mut leader_input, &mut leader_output);

    let mut follower = fixture.spawn_frontend();
    let mut follower_input = follower.stdin.take().expect("follower input");
    let mut follower_output = follower.stdout.take().expect("follower output");
    complete_handshake(&mut follower_input, &mut follower_output);

    write_build_derivation_request(&mut leader_input, "x86_64-linux", 0);
    leader_input.flush().expect("leader request flushes");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        assert!(Instant::now() < deadline, "leader helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    write_build_derivation_request(&mut follower_input, "x86_64-linux", 0);
    follower_input.flush().expect("follower request flushes");
    assert_eq!(
        read_integer(&mut follower_output),
        nix_worker_protocol::STDERR_NEXT
    );
    assert_eq!(
        read_string(&mut follower_output),
        "identical build already in progress\n"
    );

    fixture.frontend.kill().expect("leader terminates");
    fixture.frontend.wait().expect("leader reaps");
    drop(leader_input);
    drop(leader_output);
    fs::write(&complete, b"complete").expect("helper completion releases");
    follower.kill().expect("follower terminates");
    follower.wait().expect("follower reaps");
    drop(follower_input);
    drop(follower_output);
    assert_eq!(
        fs::read_to_string(&invocation_count).expect("invocation count reads"),
        "x"
    );
    fixture.finish();
    fs::remove_dir_all(root).expect("fixture cleans");
}
