//! Exercises audit delivery through the telemetry provider without worker-thread blocking.

use std::process::Command;

#[test]
fn emit_audit() {
    if std::env::var_os("TELCHAR_AUDIT_TEST").is_none() {
        return;
    }
    let telemetry = telchar_telemetry::Telemetry::initialize("audit-test", "test").unwrap();
    telchar_telemetry::emit_audit(
        "upload.accepted",
        "{\"session_id\":\"audit-fixture\",\"store_path\":\"/nix/store/fixture\"}",
    );
    if std::env::var_os("TELCHAR_AUDIT_BURST").is_some() {
        let started = std::time::Instant::now();
        for _ in 0..2000 {
            telchar_telemetry::emit_audit("upload.accepted", "{\"session_id\":\"audit-burst\"}");
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
    telemetry.shutdown();
}

#[test]
fn failed_export_falls_back_to_socket_stderr() {
    use std::io::Read;
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["emit_audit", "--exact", "--nocapture"])
        .env("TELCHAR_AUDIT_TEST", "1")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1")
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")
        .env("RUST_LOG", "off")
        .stderr(std::process::Stdio::from(std::os::fd::OwnedFd::from(
            writer,
        )))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    reader
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let mut stderr = String::new();
    reader.read_to_string(&mut stderr).unwrap();
    assert!(stderr.contains("WARN audit.export_failed"), "{stderr}");
    assert!(stderr.contains("audit-fixture"), "{stderr}");
}

#[test]
fn saturated_queue_and_blocked_stderr_do_not_stall_producer() {
    use std::io::Write;
    use std::process::Stdio;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        let _ = stream.write_all(b"HTTP/1.1 503 Unavailable\r\ncontent-length: 0\r\n\r\n");
    });
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["emit_audit", "--exact", "--nocapture"])
        .env("TELCHAR_AUDIT_TEST", "1")
        .env("TELCHAR_AUDIT_BURST", "1")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", format!("http://{address}"))
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")
        .env("RUST_LOG", "off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("audit producer/shutdown blocked on undrained stderr");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();
    assert!(exit.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("audit.queue_full"));
    server.join().unwrap();
}

#[test]
fn failed_export_falls_back_locally_without_tracing_recursion() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["emit_audit", "--exact", "--nocapture"])
        .env("TELCHAR_AUDIT_TEST", "1")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", format!("http://{address}"))
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")
        .env("RUST_LOG", "off")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("WARN audit.export_failed"), "{stderr}");
    assert!(stderr.contains("audit-fixture"), "{stderr}");
    assert!(
        !stderr.contains(&address.to_string()),
        "endpoint leaked: {stderr}"
    );
    assert_eq!(
        stderr.matches("WARN audit.export_failed").count(),
        1,
        "{stderr}"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("audit-fixture"));
}
