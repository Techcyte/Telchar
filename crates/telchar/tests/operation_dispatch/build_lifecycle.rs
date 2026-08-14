//! Tests build lease, attachment, output, and cleanup ordering.

use super::*;

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
            ("TELCHAR_TEST_PROMOTE_HELPER", helper.display().to_string()),
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
            ("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string()),
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
    let shared_build = fixture
        .database
        .connect()
        .query_one(
            "SELECT state, quota_subject FROM shared_builds WHERE derivation_path = $1",
            &[&"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"],
        )
        .expect("running shared build reads");
    assert_eq!(shared_build.get::<_, String>(0), "running");
    assert_eq!(
        shared_build.get::<_, String>(1),
        "ssh-pubkey:SHA256:fixture"
    );
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
    let completed_shared_build = fixture
        .database
        .connect()
        .query_one(
            "SELECT state FROM shared_builds WHERE derivation_path = $1",
            &[&"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"],
        )
        .expect("completed shared build reads");
    assert_eq!(completed_shared_build.get::<_, String>(0), "succeeded");
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
        "#!/bin/sh\nset -eu\nprintf '{\"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv\":{\"narHash\":\"sha256-bCvi8SoWhgXry6N4IobDjq8/XXh7eouySlQNImf/aOE=\",\"narSize\":136,\"references\":[],\"deriver\":null,\"ca\":null},\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\":{\"narHash\":\"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\",\"narSize\":136,\"references\":[],\"deriver\":null,\"ca\":null}}\\n'\n",
    )
    .expect("Nix query helper writes");
    fs::set_permissions(&nix, fs::Permissions::from_mode(0o700)).expect("Nix helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [
            (
                "TELCHAR_TEST_BUILD_HELPER",
                build_helper.display().to_string(),
            ),
            (
                "TELCHAR_TEST_EXPORT_HELPER",
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
    let shared_build_state: String = database
        .query_one(
            "SELECT state FROM shared_builds WHERE derivation_path = $1",
            &[&"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"],
        )
        .expect("shared build state reads")
        .get(0);
    assert_eq!(shared_build_state, "failed");
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
