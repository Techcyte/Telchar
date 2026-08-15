//! Tests admission and derivation/input lease ordering before backend execution.

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
