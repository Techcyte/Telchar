//! Verifies exporter transports, service identity, and correlated local diagnostics.

use std::process::{Command, Output};

#[allow(dead_code)]
#[path = "../../../tests/telemetry/collector.rs"]
mod collector;
#[path = "support/http.rs"]
mod http;

#[test]
fn emit_signals() {
    if std::env::var_os("TELCHAR_TELEMETRY_IDENTITY_TEST").is_none() {
        return;
    }
    let telemetry = telchar_telemetry::Telemetry::initialize("test-service", "identity-version")
        .expect("telemetry initializes");
    {
        let span = tracing::info_span!("request", request_id = "identity-test");
        let _entered = span.enter();
        tracing::info!(request_id = "identity-test", "service started");
        opentelemetry::global::meter("identity-test")
            .u64_counter("test.starts")
            .build()
            .add(1, &[]);
    }
    telemetry.shutdown();
}

fn export(endpoint: String, protocol: &str) -> Output {
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["emit_signals", "--exact", "--nocapture"])
        .env("TELCHAR_TELEMETRY_IDENTITY_TEST", "1")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint)
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", protocol)
        .env("RUST_LOG", "info")
        .output()
        .expect("telemetry process starts");
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(" INFO service started"), "{stderr}");
    assert!(stderr.contains(" trace_id="), "{stderr}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains(" trace_id="));
    output
}

#[test]
fn exports_configured_service_identity() {
    let collector = collector::start_collector();
    let output = export(collector.endpoint(), "grpc");
    assert!(collector.has_all_signals());
    let trace_id = collector.assert_correlated("identity-test", "test-service");
    assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("trace_id={trace_id}")));
    for request in collector.log_requests.lock().expect("log requests").iter() {
        for resource in &request.resource_logs {
            assert!(collector::Collector::has_attribute(
                &resource.resource.as_ref().expect("resource").attributes,
                "service.version",
                "identity-version"
            ));
        }
    }
}

#[test]
fn exports_otlp_signals_over_http_protobuf() {
    let collector = http::start_http_collector();
    export(collector.endpoint(), "http/protobuf");
    assert!(
        collector.has_all_signals(),
        "HTTP collector missed OTLP signals: {:?}",
        collector.paths.lock().expect("HTTP collector paths")
    );
}
