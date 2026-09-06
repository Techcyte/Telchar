//! Captures bounded daemon query telemetry without request data.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use telchar::fixture::nix::{NixFixture, TrustMode};
use telchar::store::GatewayStoreEndpoint;
use telchar::store::query::{GatewayStoreQuery, QueryValidPathsStore};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn capture(level: tracing::Level, operation: impl FnOnce()) -> String {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let writer = Capture(bytes.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(level)
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, operation);
    String::from_utf8(bytes.lock().unwrap().clone()).unwrap()
}

#[test]
fn real_query_trace_reports_daemon_phases_without_paths() {
    let fixture = NixFixture::create().unwrap();
    let mut daemon = fixture.start_daemon(TrustMode::Trusted).unwrap();
    let endpoint = GatewayStoreEndpoint::parse(&daemon.store_url()).unwrap();
    let trace = capture(tracing::Level::TRACE, || {
        let mut connection = telchar::store::GatewayStoreConnection::connect(&endpoint).unwrap();
        assert!(connection.query_valid_paths(&[], false).unwrap().is_empty());
    });
    for phase in [
        "store.daemon.connected",
        "store.daemon.handshake",
        "store.daemon.query_valid_paths",
    ] {
        assert!(trace.contains(phase), "missing {phase}: {trace}");
    }
    assert!(trace.contains("elapsed_us="), "{trace}");
    assert!(!trace.contains("sensitive-"), "{trace}");
    assert!(!trace.contains(&daemon.store_url()), "{trace}");
    assert!(!trace.contains("NIX_CONFIG"), "{trace}");
    let filtered = capture(tracing::Level::INFO, || {
        let mut connection = telchar::store::GatewayStoreConnection::connect(&endpoint).unwrap();
        connection.query_valid_paths(&[], false).unwrap();
    });
    assert!(filtered.is_empty(), "{filtered}");
    daemon.stop().unwrap();
    fixture.cleanup().unwrap();
}

#[test]
fn connection_failure_trace_is_bounded() {
    let mut query = GatewayStoreQuery::new(
        GatewayStoreEndpoint::parse("unix:///nonexistent/sensitive-socket-marker").unwrap(),
    );
    let trace = capture(tracing::Level::TRACE, || {
        let error = query
            .query_valid_paths(
                &[b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-sensitive-path-marker".to_vec()],
                false,
            )
            .unwrap_err();
        assert!(!error.to_string().contains("sensitive-"));
    });
    assert!(trace.contains("store.daemon.connected"), "{trace}");
    assert!(trace.contains("success=false"), "{trace}");
    assert!(!trace.contains("sensitive-"), "{trace}");
}

#[test]
fn real_relay_trace_marks_boundaries_not_payload_or_each_buffer() {
    let input = vec![42; telchar::service::ipc::MAX_FRONTEND_BUFFER_BYTES * 3];
    let mut output = Vec::new();
    let trace = capture(tracing::Level::TRACE, || {
        telchar::service::ipc::copy_bounded(input.as_slice(), &mut output).unwrap();
    });
    assert_eq!(output, input);
    assert_eq!(trace.matches("ipc.relay.first_bytes").count(), 1, "{trace}");
    assert_eq!(trace.matches("ipc.relay.finished").count(), 1, "{trace}");
    let filtered = capture(tracing::Level::INFO, || {
        telchar::service::ipc::copy_bounded(input.as_slice(), std::io::sink()).unwrap();
    });
    assert!(filtered.is_empty(), "{filtered}");
}
