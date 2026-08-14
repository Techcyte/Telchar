//! Tests durable reuse, backpressure, disconnect, and cancellation.

use super::*;

#[test]
fn equivalent_build_requests_keep_distinct_request_ids_and_reuse_durable_success() {
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
    assert_eq!(
        fs::read_dir(&request_directory)
            .expect("request directory reads")
            .count(),
        1,
        "durable success avoids duplicate backend execution"
    );
    let mut database = fixture.database.connect();
    let mut request_ids = database
        .query(
            "SELECT request_id FROM build_requests ORDER BY request_id",
            &[],
        )
        .expect("request IDs read")
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    request_ids.sort();
    assert_eq!(request_ids.len(), 2);
    assert_ne!(request_ids[0], request_ids[1]);
    for request_id in &request_ids {
        assert!(request_id.starts_with("request-"), "{request_id}");
        assert!(request_id.len() <= telchar::service::ipc::MAX_IPC_COMPONENT_BYTES);
        assert_eq!(
            telchar::persistence::read_build_request(fixture.database.url(), request_id)
                .expect("build request reads")
                .expect("build request persists")
                .request_id,
            *request_id
        );
    }
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
        assert!(lease_id.len() <= telchar::service::ipc::MAX_IPC_COMPONENT_BYTES);
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
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
