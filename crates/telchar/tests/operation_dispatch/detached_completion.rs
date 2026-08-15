//! Tests detached execution completion without transport authority.

use super::*;

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
        "#!/bin/sh\nset -eu\nprintf '{\"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv\":{\"narHash\":\"sha256-bCvi8SoWhgXry6N4IobDjq8/XXh7eouySlQNImf/aOE=\",\"narSize\":136,\"references\":[],\"deriver\":null,\"ca\":null},\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\":{\"narHash\":\"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\",\"narSize\":136,\"references\":[],\"deriver\":null,\"ca\":null}}\\n'\n",
    )
    .expect("Nix query helper writes");
    fs::set_permissions(&nix, fs::Permissions::from_mode(0o700)).expect("Nix helper executable");
    let mut fixture = FrontendFixture::spawn_with_store_default_disconnect_policy(
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
    wait_for_path_state_for(
        fixture.database.url(),
        "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv",
        telchar::persistence::SharedBuildState::Succeeded,
        Duration::from_secs(2),
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
