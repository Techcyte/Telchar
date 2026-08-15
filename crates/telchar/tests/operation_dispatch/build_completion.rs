//! Tests attachment and output validation ordering before terminal results.

use super::*;

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
