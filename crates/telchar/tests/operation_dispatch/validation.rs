//! Tests build admission and unsupported-operation rejection.

use super::*;

#[test]
fn valid_build_derivation_is_consumed_before_execution_unavailable_error() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-unavailable-execution-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let unavailable = root.join("build-helper");
    fs::write(&unavailable, "#!/bin/sh\nexit 1\n").expect("unavailable helper writes");
    fs::set_permissions(&unavailable, fs::Permissions::from_mode(0o700))
        .expect("unavailable helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [(
            "TELCHAR_TEST_BUILD_HELPER",
            unavailable.display().to_string(),
        )],
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
    let mut client = fixture.database.connect();
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM build_requests", &[])
            .expect("request count reads")
            .get::<_, i64>(0),
        1,
        "execution failure discarded the immutable build request"
    );
    assert_eq!(
        client
            .query_one("SELECT state FROM request_attachments", &[])
            .expect("attachment state reads")
            .get::<_, String>(0),
        "detached",
        "execution failure left attachment attached"
    );
    assert_released_derivation_lease(&fixture.database, &request_id(&mut client));
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.build_derivation.admitted"),
        "{stderr}"
    );
    assert!(
        stderr.contains("worker.build_derivation.failed"),
        "{stderr}"
    );
    assert!(!stderr.contains("printf telchar-remote-build"), "{stderr}");
}

#[test]
fn mismatched_build_derivation_system_is_rejected_before_execution() {
    let mut fixture = FrontendFixture::spawn(None);
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_gate_3_build_derivation(&mut input, "aarch64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(
        read_string(&mut output),
        "unsupported BuildDerivation request"
    );
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
        "rejected system persisted a build request"
    );
    let stderr = fixture.finish();
    assert!(stderr.contains("unsupported-build-derivation"), "{stderr}");
    assert!(
        !stderr.contains("worker.build_derivation.admitted"),
        "{stderr}"
    );
}

#[test]
fn recognized_unsupported_operation_returns_a_distinct_framed_error() {
    let response = send_operation(39);
    assert_eq!(response.message, "unsupported worker operation");
    assert_eq!(response.rejection, "recognized-unsupported");
}

#[test]
fn unknown_operation_returns_a_framed_error() {
    let response = send_operation(0xffff);
    assert_eq!(response.message, "unknown worker operation");
    assert_eq!(response.rejection, "unknown-operation");
}
