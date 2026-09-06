//! Checks connection ownership and failure handling against an isolated real daemon.

use crate::fixture::nix::{NixFixture, TrustMode};
use std::net::Shutdown;

#[test]
fn failed_import_operation_discards_stream_without_replay() {
    for _ in 0..50 {
        check_operation();
    }
}

fn check_operation() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("real daemon starts");
    let mut importer = daemon.promotion_backend().expect("importer creates");
    importer
        .with_connection(|connection| connection.query_missing(&[]))
        .expect("real query completes");
    assert!(importer.connection.is_some());
    let mut attempts = 0;
    let error = importer
        .with_connection(|connection| {
            attempts += 1;
            connection.shutdown_handle()?.shutdown(Shutdown::Both)?;
            connection.query_missing(&[])
        })
        .expect_err("closed socket fails");
    assert_eq!(error.to_string(), "gateway Nix daemon connection failed");
    assert_eq!(attempts, 1, "failed operation is not replayed");
    assert!(importer.connection.is_none(), "failed stream is discarded");
    importer
        .with_connection(|connection| connection.query_missing(&[]))
        .expect("next operation establishes a working connection");
    assert!(importer.connection.is_some());
    drop(importer);
    daemon.stop().expect("daemon stops");
    fixture.cleanup().expect("fixture cleans");
}
