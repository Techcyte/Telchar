//! Tests shared execution coalescing and leader/follower disconnect ownership.

use super::*;

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
            "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf x >> '{}'\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
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

    write_gate_3_build_derivation(&mut leader_input, "x86_64-linux", 0);
    leader_input.flush().expect("leader request flushes");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        assert!(Instant::now() < deadline, "leader helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    write_gate_3_build_derivation(&mut follower_input, "x86_64-linux", 0);
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
        "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv",
    )
    .expect("shared build reads")
    .expect("shared build exists");
    assert_eq!(
        shared_build.state,
        telchar::persistence::SharedBuildState::Succeeded
    );
    let attempt = telchar::persistence::read_shared_build_attempt(
        fixture.database.url(),
        "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv",
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
    fixture.finish();
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
            "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf x >> '{}'\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
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

    write_gate_3_build_derivation(&mut leader_input, "x86_64-linux", 0);
    leader_input.flush().expect("leader request flushes");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        assert!(Instant::now() < deadline, "leader helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    write_gate_3_build_derivation(&mut follower_input, "x86_64-linux", 0);
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
            "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf x >> '{}'\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
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

    write_gate_3_build_derivation(&mut leader_input, "x86_64-linux", 0);
    leader_input.flush().expect("leader request flushes");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        assert!(Instant::now() < deadline, "leader helper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    write_gate_3_build_derivation(&mut follower_input, "x86_64-linux", 0);
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
