//! Tests worker handshake, deadlines, and query operation dispatch.

use super::*;

#[test]
fn live_set_options_request_returns_terminal_frame() {
    let otlp_endpoint = std::env::var("TELCHAR_TEST_OTLP_ENDPOINT").ok();
    let mut fixture = FrontendFixture::spawn_configured(
        None,
        None,
        otlp_endpoint
            .as_ref()
            .into_iter()
            .map(|endpoint| ("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint.clone())),
        Some("cancel-running"),
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");

    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 19);
    for _ in 0..12 {
        write_integer(&mut input, 0);
    }
    write_integer(&mut input, 0);
    drop(input);

    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .expect("server stdout")
        .read_to_end(&mut stdout)
        .expect("server stdout reads");
    let mut expected_stdout = Vec::new();
    write_integer(&mut expected_stdout, SERVER_WORKER_MAGIC);
    write_integer(&mut expected_stdout, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut expected_stdout, 0);
    write_string(&mut expected_stdout, b"telchar");
    write_integer(&mut expected_stdout, 0);
    write_integer(&mut expected_stdout, STDERR_LAST);
    write_integer(&mut expected_stdout, STDERR_LAST);
    assert_eq!(
        stdout, expected_stdout,
        "worker stdout has no text contamination"
    );

    assert!(child.wait().expect("Telchar exits").success());
    let deadline = Instant::now() + Duration::from_secs(4);
    while fixture
        .daemon
        .try_wait()
        .expect("daemon status reads")
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "daemon did not finish telemetry shutdown"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.set_options.completed"),
        "missing local SetOptions telemetry: {stderr}"
    );
}

#[test]
fn partial_set_options_times_out_after_operation_and_cleans_up() {
    let mut fixture = FrontendFixture::spawn(Some(40));
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    input.flush().expect("handshake flushes");
    let mut output = child.stdout.take().expect("server output");
    assert_eq!(read_integer(&mut output), SERVER_WORKER_MAGIC);
    assert_eq!(read_integer(&mut output), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_integer(&mut output), 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().expect("post-handshake flushes");
    assert_eq!(read_string(&mut output), "telchar");
    assert_eq!(read_integer(&mut output), 0);
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    write_integer(&mut input, 19);
    input.flush().expect("operation flushes");
    let started = std::time::Instant::now();
    let elapsed = started.elapsed();
    let status = child.wait().expect("Telchar exits");
    assert!(elapsed < Duration::from_secs(1));
    assert!(status.success());
    let stderr = fixture.finish();
    assert!(stderr.contains("worker.session.timed_out"), "{stderr}");
}

#[test]
fn complete_message_boundary_remains_idle_until_next_input_starts() {
    let mut fixture = FrontendFixture::spawn(Some(40));
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    input.flush().expect("handshake flushes");
    let mut output = child.stdout.take().expect("server output");
    assert_eq!(read_integer(&mut output), SERVER_WORKER_MAGIC);
    assert_eq!(read_integer(&mut output), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_integer(&mut output), 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().expect("post-handshake flushes");
    assert_eq!(read_string(&mut output), "telchar");
    assert_eq!(read_integer(&mut output), 0);
    assert_eq!(read_integer(&mut output), STDERR_LAST);

    thread::sleep(Duration::from_millis(80));
    assert!(
        child.try_wait().expect("frontend status").is_none(),
        "complete-boundary idle session timed out"
    );

    input
        .write_all(&19_u64.to_le_bytes()[..1])
        .expect("partial operation starts");
    input.flush().expect("partial operation flushes");
    assert!(child.wait().expect("frontend exits").success());
    let stderr = fixture.finish();
    assert!(stderr.contains("worker.session.timed_out"), "{stderr}");
}

#[test]
fn partial_set_options_progress_resets_deadline() {
    let mut fixture = FrontendFixture::spawn(Some(40));
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    input.flush().expect("handshake flushes");
    let mut output = child.stdout.take().expect("server output");
    assert_eq!(read_integer(&mut output), SERVER_WORKER_MAGIC);
    assert_eq!(read_integer(&mut output), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_integer(&mut output), 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().expect("post-handshake flushes");
    assert_eq!(read_string(&mut output), "telchar");
    assert_eq!(read_integer(&mut output), 0);
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    write_integer(&mut input, 19);
    write_integer(&mut input, 0);
    input.flush().expect("partial request progresses");
    std::thread::sleep(Duration::from_millis(25));
    for _ in 0..11 {
        write_integer(&mut input, 0);
    }
    write_integer(&mut input, 0);
    input.flush().expect("request completes");
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    fixture.finish();
}
