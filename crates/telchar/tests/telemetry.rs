//! Verifies gateway metric instruments and protocol-safe diagnostic output.

use std::process::Command;
use std::time::{Duration, Instant};

#[allow(dead_code)]
#[path = "../../../tests/telemetry/collector.rs"]
mod collector;
use collector::{Collector, start_collector};

const SERVICE_NAME: &str = "telchar";

fn run_smoke(endpoint: String, protocol: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_telchar"))
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint)
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", protocol)
        .env("TELCHAR_SMOKE_REQUEST_ID", "request-smoke-001")
        .env("TELCHAR_SMOKE_ERROR", "1")
        .env("TELCHAR_SMOKE_OPERATIONAL_METRICS", "1")
        .env("TELCHAR_SMOKE_TRACE_PARENT", "1")
        .output()
        .expect("Telchar process starts")
}

fn assert_local_smoke_output(output: &std::process::Output) -> String {
    assert!(
        output.status.success(),
        "Telchar process failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Nix worker protocol\n"),
        "missing protocol output: {output:?}"
    );
    assert!(
        !stdout.contains(" trace_id="),
        "local telemetry contaminated command output: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(" trace_id="),
        "missing local trace ID: {output:?}"
    );
    assert!(
        stderr.contains(" INFO request started"),
        "missing local log: {output:?}"
    );
    assert!(
        stderr.contains(" ERROR smoke error"),
        "missing local error: {output:?}"
    );

    stderr.into_owned()
}

fn assert_grpc_signals(collector: &Collector, stderr: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !collector.has_all_signals() {
        std::thread::sleep(Duration::from_millis(10));
    }

    let trace_id = collector.assert_correlated("request-smoke-001", SERVICE_NAME);
    collector.assert_span_parent("smoke.parent", "smoke.child");
    assert!(stderr.contains(&format!("trace_id={trace_id}")));
    let names = collector.metric_names();
    for expected in [
        "telchar.build.requests",
        "telchar.build.request.duration",
        "telchar.service.session.rejections",
        "telchar.shared_build.queue.depth",
        "telchar.shared_build.follower.wait.duration",
        "telchar.backend.permits.active",
        "telchar.backend.permits.waiting",
        "telchar.backend.permit.wait.duration",
        "telchar.static_ssh.health.checks",
        "telchar.static_ssh.health.check.duration",
        "telchar.configuration.reload.duration",
        "telchar.configuration.reload.static_ssh.added",
        "telchar.configuration.reload.static_ssh.removed",
        "telchar.cache.substitutions",
        "telchar.transfer.active",
        "telchar.transfer.bytes",
        "telchar.transfer.failures",
        "telchar.recovery.attempts",
        "telchar.recovery.duration",
        "telchar.recovery.outcomes",
        "telchar.recovery.monitoring",
        "telchar.nomad.pending",
    ] {
        assert!(
            names.contains(expected),
            "missing OTLP metric {expected}: {names:?}"
        );
    }
    collector.assert_metric_attributes_are_bounded();
}

#[test]
fn frontend_exports_correlation_without_changing_protocol_bytes() {
    frontend_relay(false);
}

#[test]
fn frontend_collector_outage_preserves_relay_and_bounded_exit() {
    frontend_relay(true);
}

fn frontend_relay(outage: bool) {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::process::Stdio;

    let collector = start_collector();
    let root = std::env::temp_dir().join(format!("telchar-front-{}-{outage}", std::process::id()));
    std::fs::create_dir(&root).expect("isolated frontend directory creates");
    let socket = root.join("ipc");
    let config = root.join("service.toml");
    std::fs::write(&config, "").unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .arg("serve-stdio")
        .env("TELCHAR_CONFIG", &config)
        .env("TELCHAR_IPC_SOCKET", &socket)
        .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:fixture")
        .env(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            if outage {
                "http://127.0.0.1:9".to_owned()
            } else {
                collector.endpoint()
            },
        )
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
        .env("RUST_LOG", "trace")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    let output = child.wait_with_output().unwrap();
                    panic!(
                        "frontend connect deadline: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("frontend IPC accept: {error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut length = [0; 4];
    stream.read_exact(&mut length).unwrap();
    let length = u32::from_le_bytes(length) as usize;
    assert!(length < 65536);
    let mut envelope = vec![0; length];
    stream.read_exact(&mut envelope).unwrap();
    let envelope = telchar::service::ipc::IpcEnvelope::decode(&envelope).unwrap();
    assert!(envelope.trace_context.trace_id().is_some());
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"request-bytes")
        .unwrap();
    let mut request = Vec::new();
    stream.read_to_end(&mut request).unwrap();
    assert_eq!(request, b"request-bytes");
    stream.write_all(b"response-bytes").unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let shutdown = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if shutdown.elapsed() >= Duration::from_secs(5) {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "frontend shutdown deadline: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"response-bytes");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("ipc.frontend.envelope_sent"));
    if outage {
        assert!(collector.spans("telchar", "ipc.frontend").is_empty());
    } else {
        assert!(collector.has_log_event("ipc.frontend.envelope_sent"));
        assert!(!collector.spans("telchar", "ipc.frontend").is_empty());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_otlp_signals_before_application_work() {
    let collector = start_collector();
    let output = run_smoke(collector.endpoint(), "grpc");
    let stderr = assert_local_smoke_output(&output);
    assert_grpc_signals(&collector, &stderr);
}
