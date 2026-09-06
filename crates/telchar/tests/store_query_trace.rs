//! Captures bounded query telemetry from real Nix subprocesses and filesystem failures.

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
fn real_query_trace_reports_phases_without_paths_or_environment() {
    let fixture = NixFixture::create().unwrap();
    let mut daemon = fixture.start_daemon(TrustMode::Trusted).unwrap();
    let source = fixture.temp_dir().join("sensitive-source-marker");
    std::fs::write(&source, "sensitive-content-marker").unwrap();
    let added = std::process::Command::new("nix-store")
        .args(["--store", &daemon.store_url(), "--add"])
        .arg(&source)
        .envs(fixture.environment())
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let path = String::from_utf8(added.stdout)
        .unwrap()
        .trim()
        .as_bytes()
        .to_vec();
    let mut query = GatewayStoreQuery::with_endpoint_and_environment(
        "nix",
        Some(GatewayStoreEndpoint::parse(&daemon.store_url()).unwrap()),
        fixture
            .environment()
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value)),
    );
    let trace = capture(tracing::Level::TRACE, || {
        assert_eq!(
            query
                .query_valid_paths(std::slice::from_ref(&path))
                .unwrap(),
            vec![path.clone()]
        );
    });
    for phase in [
        "store.query.spawn",
        "store.query.wait",
        "store.query.drain",
        "store.query.parse",
    ] {
        assert!(trace.contains(phase), "missing {phase}: {trace}");
    }
    assert!(trace.contains("elapsed_us="), "{trace}");
    assert!(!trace.contains("sensitive-"), "{trace}");
    assert!(!trace.contains(&daemon.store_url()), "{trace}");
    assert!(!trace.contains("NIX_CONFIG"), "{trace}");
    let daemon_trace = capture(tracing::Level::TRACE, || {
        let endpoint = GatewayStoreEndpoint::parse(&daemon.store_url()).unwrap();
        telchar::store::GatewayStoreConnection::connect(&endpoint).unwrap();
    });
    assert!(
        daemon_trace.contains("store.daemon.connected"),
        "{daemon_trace}"
    );
    assert!(
        daemon_trace.contains("store.daemon.handshake"),
        "{daemon_trace}"
    );
    assert!(
        !daemon_trace.contains(&daemon.store_url()),
        "{daemon_trace}"
    );
    let filtered = capture(tracing::Level::INFO, || {
        query.query_valid_paths(&[path]).unwrap();
    });
    assert!(filtered.is_empty(), "{filtered}");
    daemon.stop().unwrap();
    fixture.cleanup().unwrap();
}

#[test]
fn spawn_failure_trace_is_bounded_and_filtered() {
    let mut query = GatewayStoreQuery::new(
        "/nonexistent/sensitive-executable-marker",
        GatewayStoreEndpoint::parse("unix:///nonexistent/sensitive-socket-marker").unwrap(),
    );
    let trace = capture(tracing::Level::TRACE, || {
        assert!(
            query
                .query_valid_paths(&[
                    b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-sensitive-path-marker".to_vec()
                ])
                .is_err()
        );
    });
    assert!(trace.contains("store.query.spawn"), "{trace}");
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
    assert!(trace.contains("elapsed_us="), "{trace}");
    let filtered = capture(tracing::Level::INFO, || {
        telchar::service::ipc::copy_bounded(input.as_slice(), std::io::sink()).unwrap();
    });
    assert!(filtered.is_empty(), "{filtered}");
}

#[test]
fn real_nix_failure_does_not_export_subprocess_stderr() {
    let mut query = GatewayStoreQuery::new(
        "nix",
        GatewayStoreEndpoint::parse("unix:///nonexistent/sensitive-socket-marker").unwrap(),
    );
    let trace = capture(tracing::Level::TRACE, || {
        assert!(
            query
                .query_valid_paths(&[
                    b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-sensitive-path-marker".to_vec()
                ])
                .is_err()
        );
    });
    assert!(trace.contains("store.query.wait"), "{trace}");
    assert!(trace.contains("success=false"), "{trace}");
    assert!(trace.contains("stderr_bytes="), "{trace}");
    assert!(!trace.contains("sensitive-"), "{trace}");
}
