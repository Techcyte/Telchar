//! Tests terminal rollback, detach, and root release failure ordering.

use super::*;

#[test]
fn output_lease_failure_rolls_back_output_root_before_request_cleanup() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-output-lease-failure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let started = root.join("started");
    let release = root.join("release");
    let response = r#"{"version":1,"success":true,"status":"built","outputs":[["out","/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract"]]}"#;
    fs::write(&helper, format!("#!/bin/sh\nset -eu\ncat >/dev/null\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '%s\n' '{}'\n", started.display(), release.display(), response))
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_multi_with_store(
        "unix:///fixed-gateway.sock",
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
    );
    let mut input = fixture.frontend.stdin.take().expect("server input");
    let mut output = fixture.frontend.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);
    write_build_derivation_request(&mut input, "x86_64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");

    wait_for_file_for(&started, Duration::from_secs(5), "leader not started");
    let leader_request = request_id(&mut fixture.database.connect());
    fixture.database.connect().batch_execute(&format!(
        "CREATE FUNCTION reject_output_lease() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.purpose = 'output' AND NEW.owner_id = '{leader_request}' THEN RAISE EXCEPTION 'reject output lease'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_output_lease BEFORE INSERT ON store_leases FOR EACH ROW EXECUTE FUNCTION reject_output_lease();"
    )).unwrap();
    let mut follower = fixture.spawn_frontend();
    let mut follower_input = follower.stdin.take().unwrap();
    let mut follower_output = follower.stdout.take().unwrap();
    complete_handshake(&mut follower_input, &mut follower_output);
    write_build_derivation_request(&mut follower_input, "x86_64-linux", 0);
    follower_input.flush().unwrap();
    assert_eq!(
        read_integer(&mut follower_output),
        nix_worker_protocol::STDERR_NEXT
    );
    assert_eq!(
        read_string(&mut follower_output),
        "identical build already in progress\n"
    );
    fs::write(&release, "release").unwrap();
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
    assert!(fixture.frontend.wait().expect("Telchar exits").success());

    assert_eq!(
        read_integer(&mut follower_output),
        STDERR_ERROR,
        "failed durable owner must not authorize follower success"
    );
    assert_eq!(read_string(&mut follower_output), "Error");
    read_integer(&mut follower_output);
    assert_eq!(read_string(&mut follower_output), "Error");
    assert_eq!(
        read_string(&mut follower_output),
        "BuildDerivation execution failed"
    );
    assert_eq!(read_integer(&mut follower_output), 0);
    assert_eq!(read_integer(&mut follower_output), 0);
    drop(follower_input);
    assert!(follower.wait().unwrap().success());
    let mut database = fixture.database.connect();
    let request_id = leader_request;
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
            &[&"/nix/store/00000000000000000000000000000000-telchar-build-derivation-contract.drv"],
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
        !stderr.contains(
            "/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract"
        ),
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
        "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract\"]]}\\n'\n",
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
    write_build_derivation_request(&mut input, "x86_64-linux", 0);
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
            "#!/bin/sh\nset -eu\nprintf started > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract\"]]}}\\n'\n",
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
    write_build_derivation_request(&mut input, "x86_64-linux", 0);
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
