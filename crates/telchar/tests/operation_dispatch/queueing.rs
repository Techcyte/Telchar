//! Tests subject admission, queued ownership, and backend permit separation.

use super::*;

#[test]
fn saturated_subject_waits_while_another_subject_builds() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-subject-dispatch-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let alice_started = root.join("alice-started");
    let alice_complete = root.join("alice-complete");
    let bob_started = root.join("bob-started");
    let bob_complete = root.join("bob-complete");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nrequest=$(cat)\ncase \"$request\" in\n  *00000000000000000000000000000000-telchar-gate-3-contract.drv*) started='{}'; complete='{}' ;;\n  *bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-telchar-gate-3-contract.drv*) started='{}'; complete='{}' ;;\n  *) exit 1 ;;\nesac\nprintf started > \"$started\"\nwhile [ ! -e \"$complete\" ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            alice_started.display(),
            alice_complete.display(),
            bob_started.display(),
            bob_complete.display(),
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_multi_with_store_capacity(
        "unix:///fixed-gateway.sock",
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
        1,
    );
    let retained_derivation_path =
        "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv";
    for index in 0..4 {
        let derivation_path = format!("/nix/store/{index:032x}-alice-active-{index}.drv");
        telchar::persistence::claim_shared_build(
            fixture.database.url(),
            &derivation_path,
            &[index as u8; 32],
            "local",
            telchar::backend::BackendKind::Local,
            telchar::backend::BackendKind::Local.capabilities(),
            None,
            &["/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"],
        )
        .expect("active Alice build claims");
        telchar::persistence::enqueue_shared_build(
            fixture.database.url(),
            &derivation_path,
            "ssh-pubkey:SHA256:fixture",
            64,
        )
        .expect("active Alice build enqueues");
        telchar::persistence::start_queued_shared_build(
            fixture.database.url(),
            &derivation_path,
            4,
        )
        .expect("active Alice build starts");
    }

    let mut alice_input = fixture.frontend.stdin.take().expect("Alice input");
    let mut alice_output = fixture.frontend.stdout.take().expect("Alice output");
    complete_handshake(&mut alice_input, &mut alice_output);
    let mut bob = fixture.spawn_frontend_with_key("SHA256:bob");
    let mut bob_input = bob.stdin.take().expect("Bob input");
    let mut bob_output = bob.stdout.take().expect("Bob output");
    complete_handshake(&mut bob_input, &mut bob_output);

    write_build_derivation(
        &mut alice_input,
        retained_derivation_path.as_bytes(),
        "x86_64-linux",
        0,
    );
    alice_input.flush().expect("Alice request flushes");
    wait_for_path_state(
        fixture.database.url(),
        retained_derivation_path,
        telchar::persistence::SharedBuildState::Claimed,
    );
    assert_eq!(
        shared_build_quota_subject(&fixture.database, retained_derivation_path),
        "ssh-pubkey:SHA256:fixture"
    );
    assert!(!alice_started.exists(), "saturated Alice build started");

    write_build_derivation(
        &mut bob_input,
        b"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-telchar-gate-3-contract.drv",
        "x86_64-linux",
        0,
    );
    bob_input.flush().expect("Bob request flushes");
    wait_for_file_for(
        &bob_started,
        Duration::from_secs(10),
        "Bob helper did not start",
    );
    assert_eq!(
        shared_build_quota_subject(
            &fixture.database,
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-telchar-gate-3-contract.drv",
        ),
        "ssh-pubkey:SHA256:bob"
    );
    assert!(!alice_started.exists(), "Alice bypassed subject capacity");
    fs::write(&bob_complete, b"complete").expect("Bob completion releases");
    read_build_success(&mut bob_output).expect("Bob build succeeds");
    drop(bob_input);
    assert!(bob.wait().expect("Bob exits").success());

    telchar::persistence::complete_shared_build_failure(
        fixture.database.url(),
        "/nix/store/00000000000000000000000000000000-alice-active-0.drv",
        "fixture-complete",
        &serde_json::json!({"stage": "test"}),
        Duration::from_secs(60),
    )
    .expect("Alice capacity releases");
    wait_for_file_for(
        &alice_started,
        Duration::from_secs(10),
        "Alice helper did not start after capacity release",
    );
    fs::write(&alice_complete, b"complete").expect("Alice completion releases");
    read_build_success(&mut alice_output).expect("Alice build succeeds");
    drop(alice_input);
    assert!(fixture.frontend.wait().expect("Alice exits").success());
    fixture.finish();
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn disconnected_queued_owner_retains_allocation_and_executes_after_capacity_release() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-queued-owner-disconnect-{}-{}",
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
            "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            started.display(),
            complete.display(),
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_multi_with_store_capacity(
        "unix:///fixed-gateway.sock",
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
        1,
    );
    let derivation_path = "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv";
    for index in 0..4 {
        let active_path = format!("/nix/store/{index:032x}-owner-active-{index}.drv");
        telchar::persistence::claim_shared_build(
            fixture.database.url(),
            &active_path,
            &[index as u8; 32],
            "local",
            telchar::backend::BackendKind::Local,
            telchar::backend::BackendKind::Local.capabilities(),
            None,
            &["/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"],
        )
        .expect("active owner build claims");
        telchar::persistence::enqueue_shared_build(
            fixture.database.url(),
            &active_path,
            "ssh-pubkey:SHA256:fixture",
            64,
        )
        .expect("active owner build enqueues");
        telchar::persistence::start_queued_shared_build(fixture.database.url(), &active_path, 4)
            .expect("active owner build starts");
    }

    let mut input = fixture.frontend.stdin.take().expect("owner input");
    let mut output = fixture.frontend.stdout.take().expect("owner output");
    complete_handshake(&mut input, &mut output);
    write_gate_3_build_derivation(&mut input, "x86_64-linux", 0);
    input.flush().expect("owner request flushes");
    wait_for_path_state(
        fixture.database.url(),
        derivation_path,
        telchar::persistence::SharedBuildState::Claimed,
    );
    assert_eq!(
        shared_build_quota_subject(&fixture.database, derivation_path),
        "ssh-pubkey:SHA256:fixture"
    );
    assert!(
        !started.exists(),
        "queued owner started while quota saturated"
    );

    fixture.frontend.kill().expect("owner frontend terminates");
    fixture.frontend.wait().expect("owner frontend reaps");
    drop(input);
    drop(output);
    telchar::persistence::complete_shared_build_failure(
        fixture.database.url(),
        "/nix/store/00000000000000000000000000000000-owner-active-0.drv",
        "fixture-complete",
        &serde_json::json!({"stage": "test"}),
        Duration::from_secs(60),
    )
    .expect("owner capacity releases");
    wait_for_file_for(
        &started,
        Duration::from_secs(10),
        "detached queued owner did not start",
    );
    fs::write(&complete, b"complete").expect("helper completion releases");
    wait_for_path_state_for(
        fixture.database.url(),
        derivation_path,
        telchar::persistence::SharedBuildState::Succeeded,
        Duration::from_secs(10),
    );
    let attempt =
        telchar::persistence::read_shared_build_attempt(fixture.database.url(), derivation_path)
            .expect("detached attempt reads")
            .expect("detached attempt exists");
    assert_eq!(attempt.ordinal, 1);
    assert_eq!(
        attempt.state,
        telchar::persistence::SharedBuildAttemptState::Succeeded
    );
    let outcome = telchar::persistence::read_shared_build_attempt_outcome(
        fixture.database.url(),
        &attempt.attempt_id,
    )
    .expect("detached outcome reads")
    .expect("detached outcome exists");
    assert_eq!(outcome.classification, "succeeded");
    fixture.finish();
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn backend_permit_wait_is_separate_from_subject_admission() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-backend-permit-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let alice_started = root.join("alice-started");
    let alice_complete = root.join("alice-complete");
    let bob_started = root.join("bob-started");
    let bob_complete = root.join("bob-complete");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nrequest=$(cat)\ncase \"$request\" in\n  *00000000000000000000000000000000-telchar-gate-3-contract.drv*) started='{}'; complete='{}' ;;\n  *bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-telchar-gate-3-contract.drv*) started='{}'; complete='{}' ;;\n  *) exit 1 ;;\nesac\nprintf started > \"$started\"\nwhile [ ! -e \"$complete\" ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            alice_started.display(),
            alice_complete.display(),
            bob_started.display(),
            bob_complete.display(),
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_multi_with_store_capacity(
        "unix:///fixed-gateway.sock",
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
        1,
    );
    let alice_path = "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv";
    let bob_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-telchar-gate-3-contract.drv";
    let mut alice_input = fixture.frontend.stdin.take().expect("Alice input");
    let mut alice_output = fixture.frontend.stdout.take().expect("Alice output");
    complete_handshake(&mut alice_input, &mut alice_output);
    let mut bob = fixture.spawn_frontend_with_key("SHA256:bob");
    let mut bob_input = bob.stdin.take().expect("Bob input");
    let mut bob_output = bob.stdout.take().expect("Bob output");
    complete_handshake(&mut bob_input, &mut bob_output);

    write_build_derivation(&mut alice_input, alice_path.as_bytes(), "x86_64-linux", 0);
    alice_input.flush().expect("Alice request flushes");
    wait_for_file_for(
        &alice_started,
        Duration::from_secs(10),
        "Alice helper did not start",
    );

    write_build_derivation(&mut bob_input, bob_path.as_bytes(), "x86_64-linux", 0);
    bob_input.flush().expect("Bob request flushes");
    wait_for_path_state(
        fixture.database.url(),
        bob_path,
        telchar::persistence::SharedBuildState::Running,
    );
    assert_eq!(
        shared_build_quota_subject(&fixture.database, alice_path),
        "ssh-pubkey:SHA256:fixture"
    );
    assert_eq!(
        shared_build_quota_subject(&fixture.database, bob_path),
        "ssh-pubkey:SHA256:bob"
    );
    assert!(
        !bob_started.exists(),
        "Bob bypassed backend permit capacity"
    );
    let bob_attempt =
        telchar::persistence::read_shared_build_attempt(fixture.database.url(), bob_path)
            .expect("Bob attempt reads")
            .expect("Bob attempt exists");
    assert_eq!(
        bob_attempt.state,
        telchar::persistence::SharedBuildAttemptState::Running
    );

    fs::write(&alice_complete, b"complete").expect("Alice completion releases");
    read_build_success(&mut alice_output).expect("Alice build succeeds");
    drop(alice_input);
    assert!(fixture.frontend.wait().expect("Alice exits").success());
    wait_for_file_for(
        &bob_started,
        Duration::from_secs(10),
        "Bob helper did not start after permit release",
    );
    fs::write(&bob_complete, b"complete").expect("Bob completion releases");
    read_build_success(&mut bob_output).expect("Bob build succeeds");
    drop(bob_input);
    assert!(bob.wait().expect("Bob exits").success());
    fixture.finish();
    fs::remove_dir_all(root).expect("fixture cleans");
}
