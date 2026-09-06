//! Verifies worker failure export and credential redaction before process exit.

use std::process::Command;

#[allow(dead_code)]
#[path = "../../../tests/telemetry/collector.rs"]
mod collector;
use collector::{Collector, start_collector};

#[test]
fn worker_flushes_failure_telemetry_before_exit() {
    let collector = start_collector();
    let output = Command::new(env!("CARGO_BIN_EXE_telchar-nomad-worker"))
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint())
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
        .env("TELCHAR_TRANSFER_ENDPOINT", "private-marker://secret")
        .env("RUST_LOG", "info")
        .output()
        .expect("worker runs");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("worker.configuration.failed"), "{stderr}");
    assert!(!stderr.contains("private-marker"), "{stderr}");
    assert!(
        collector.has_all_signals(),
        "worker must flush logs, spans and phase metrics"
    );
    assert!(collector.has_log_event("worker.failed"));
    for request in collector.log_requests.lock().expect("logs").iter() {
        for resource in &request.resource_logs {
            assert!(Collector::has_service_name(
                resource.resource.as_ref(),
                "telchar-nomad-worker"
            ));
        }
    }
    collector.assert_metric_attributes_are_bounded();
}
