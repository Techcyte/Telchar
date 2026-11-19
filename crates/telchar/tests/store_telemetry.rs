//! Exercises exported store operation telemetry through real daemon connections.

#[allow(dead_code)]
#[path = "../../../tests/telemetry/collector.rs"]
mod collector;

use telchar::fixture::nix::{NixFixture, TrustMode};
use telchar::store::{GatewayStoreConnection, GatewayStoreEndpoint};

#[test]
fn store_operations_export_bounded_timings_and_failures() {
    let name = "store_operations_export_bounded_timings_and_failures";
    if !telchar_test_support::process::is_child(name) {
        let fixture = NixFixture::create().unwrap();
        let mut daemon = fixture.start_daemon(TrustMode::Trusted).unwrap();
        let collector = collector::start_collector();
        telchar_test_support::process::assert_success(
            telchar_test_support::process::command(name)
                .env("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint())
                .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
                .env("RUST_LOG", "info")
                .env("TELCHAR_TEST_STORE_URI", daemon.store_url()),
        );
        for operation in ["connect", "handshake", "query_valid_paths", "query_missing"] {
            collector.assert_histogram_attributes(
                "telchar.store.operation.duration",
                &[("operation", operation), ("outcome", "success")],
                0.0,
            );
        }
        collector.assert_histogram_attributes(
            "telchar.store.operation.duration",
            &[("operation", "connect"), ("outcome", "failure")],
            0.0,
        );
        collector.assert_sum_attributes(
            "telchar.store.operations",
            &[("operation", "connect"), ("outcome", "failure")],
            1,
        );
        collector.assert_sum_attributes(
            "telchar.store.operations",
            &[("operation", "query_valid_paths"), ("outcome", "success")],
            1,
        );
        for operation in ["connect", "handshake", "query_valid_paths", "query_missing"] {
            collector.assert_sum_attributes(
                "telchar.store.operations.active",
                &[("operation", operation)],
                0,
            );
        }
        collector.assert_span_parent("store.test", "store.operation");
        collector.assert_no_private_markers(&[
            "sensitive-marker",
            "sensitive-socket-marker",
            &daemon.store_url(),
        ]);
        collector.assert_duration_resolution("telchar.store.operation.duration");
        collector.assert_metric_attributes_are_bounded();
        daemon.stop().unwrap();
        fixture.cleanup().unwrap();
        return;
    }
    let store_url = std::env::var("TELCHAR_TEST_STORE_URI").unwrap();
    let telemetry = telchar_telemetry::Telemetry::initialize("telchar", "test").unwrap();
    let parent = tracing::info_span!("store.test");
    let entered = parent.enter();
    let endpoint = GatewayStoreEndpoint::parse(&store_url).unwrap();
    let mut connection = GatewayStoreConnection::connect(&endpoint).unwrap();
    assert!(connection.query_valid_paths(&[], false).unwrap().is_empty());
    assert!(connection.query_missing(&[]).unwrap().unknown.is_empty());
    let missing =
        GatewayStoreEndpoint::parse("unix:///nonexistent/sensitive-socket-marker").unwrap();
    assert!(GatewayStoreConnection::connect(&missing).is_err());
    drop(connection);
    drop(entered);
    drop(parent);
    telemetry.shutdown();
}
