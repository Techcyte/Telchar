//! Verifies database timing attribution without exporting SQL or parameters.

#[allow(dead_code)]
#[path = "../../../tests/telemetry/collector.rs"]
mod collector;

#[test]
fn database_operations_export_checkout_validation_and_query_timings() {
    let name = "database_operations_export_checkout_validation_and_query_timings";
    if !telchar_test_support::process::is_child(name) {
        let fixture = telchar_test_support::postgres::PostgresFixture::start();
        let collector = collector::start_collector();
        telchar_test_support::process::assert_success(
            telchar_test_support::process::command(name)
                .env("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint())
                .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
                .env("RUST_LOG", "info")
                .env("TELCHAR_TEST_DATABASE_URL", fixture.url()),
        );
        for operation in ["checkout", "validation", "query_one"] {
            collector.assert_histogram_attributes(
                "telchar.database.operation.duration",
                &[("operation", operation), ("outcome", "success")],
                0.0,
            );
        }
        collector.assert_histogram_attributes(
            "telchar.database.operation.duration",
            &[("operation", "batch_execute"), ("outcome", "failure")],
            0.0,
        );
        collector.assert_histogram_attributes(
            "telchar.database.lifecycle.duration",
            &[("operation", "read_shared_build")],
            0.0,
        );
        collector.assert_no_private_markers(&[
            "private-query-marker",
            "private-build-marker",
            "private_missing_table_marker",
            fixture.url(),
        ]);
        collector.assert_metric_attributes_are_bounded();
        return;
    }
    let database_url = std::env::var("TELCHAR_TEST_DATABASE_URL").unwrap();
    let telemetry = telchar_telemetry::Telemetry::initialize("telchar", "test").unwrap();
    let database = telchar::persistence::Database::connect(&database_url).unwrap();
    let mut connection = database.connection().unwrap();
    assert_eq!(
        connection
            .query_one("SELECT $1::text", &[&"private-query-marker"])
            .unwrap()
            .get::<_, String>(0),
        "private-query-marker"
    );
    let error = connection
        .batch_execute("SELECT * FROM private_missing_table_marker")
        .unwrap_err();
    assert_eq!(
        error.code(),
        Some(&postgres::error::SqlState::UNDEFINED_TABLE)
    );
    drop(connection);
    telchar::persistence::migrate(&database).unwrap();
    assert!(
        telchar::persistence::read_shared_build(&database, "private-build-marker")
            .unwrap()
            .is_none()
    );
    drop(database);
    telemetry.shutdown();
}
