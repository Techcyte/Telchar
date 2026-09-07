//! Tests durable request identity and persistence failure ordering.

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
            "#!/bin/sh\nset -eu\nrequest=$(mktemp '{}'/request-XXXXXX)\ncat > \"$request\"\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract\"]]}}\\n'\n",
            request_directory.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let socket = root.join("store.sock");
    let store = spawn_cached_output_query_daemon(&socket);
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        &format!("unix://{}", socket.display()),
        [
            ("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string()),
            ("TELCHAR_TEST_STORE_RETENTION", "filesystem-only".to_owned()),
        ],
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    for expected_status in [0, 2] {
        write_build_derivation_request(&mut input, "x86_64-linux", 0);
        input.flush().expect("BuildDerivation request flushes");
        assert_eq!(read_integer(&mut output), STDERR_LAST);
        assert_eq!(read_integer(&mut output), expected_status, "build status");
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
    assert_eq!(leases.len(), 1, "only execution retains the derivation");
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
    store.join().expect("store daemon joins");
    fs::remove_dir_all(root).expect("fixture cleans");
}

fn spawn_cached_output_query_daemon(socket: &std::path::Path) -> thread::JoinHandle<()> {
    let listener = UnixListener::bind(socket).expect("store daemon socket binds");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("store daemon accepts");
        assert_eq!(read_integer(&mut stream), CLIENT_WORKER_MAGIC);
        assert_eq!(read_integer(&mut stream), LATEST_WORKER_VERSION.to_wire());
        write_integer(&mut stream, SERVER_WORKER_MAGIC);
        write_integer(&mut stream, LATEST_WORKER_VERSION.to_wire());
        stream.flush().expect("store greeting flushes");
        assert_eq!(read_integer(&mut stream), 0);
        write_integer(&mut stream, 0);
        stream.flush().expect("store features flush");
        assert_eq!(read_integer(&mut stream), 0);
        assert_eq!(read_integer(&mut stream), 0);
        write_string(&mut stream, b"2.34.8");
        write_integer(&mut stream, 1);
        write_integer(&mut stream, STDERR_LAST);
        stream.flush().expect("store handshake flushes");
        assert_eq!(read_integer(&mut stream), 31, "QueryValidPaths operation");
        assert_eq!(read_integer(&mut stream), 1, "one expected output");
        let path = read_string(&mut stream);
        assert_eq!(read_integer(&mut stream), 0, "substitution disabled");
        write_integer(&mut stream, STDERR_LAST);
        write_integer(&mut stream, 1);
        write_string(&mut stream, path.as_bytes());
        stream.flush().expect("valid paths response flushes");
    })
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

    write_build_derivation_request(&mut input, "x86_64-linux", 0);
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

    write_build_derivation_request(&mut input, "x86_64-linux", 0);
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
    write_build_derivation_request(&mut input, "x86_64-linux", 0);
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
