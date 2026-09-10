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
fn exports_otlp_signals_before_application_work() {
    let collector = start_collector();
    let output = run_smoke(collector.endpoint(), "grpc");
    let stderr = assert_local_smoke_output(&output);
    assert_grpc_signals(&collector, &stderr);
}
