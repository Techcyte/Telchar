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
    assert!(added.status.success(), "{}", String::from_utf8_lossy(&added.stderr));
    let path = String::from_utf8(added.stdout).unwrap().trim().as_bytes().to_vec();
    let mut query = GatewayStoreQuery::with_endpoint_and_environment(
        "nix",
        Some(GatewayStoreEndpoint::parse(&daemon.store_url()).unwrap()),
        fixture.environment().into_iter().map(|(key, value)| (key.to_owned(), value)),
    );
    let trace = capture(tracing::Level::TRACE, || {
        assert_eq!(query.query_valid_paths(std::slice::from_ref(&path)).unwrap(), vec![path.clone()]);
    });
    for phase in ["store.query.spawn", "store.query.wait", "store.query.drain", "store.query.parse"] {
        assert!(trace.contains(phase), "missing {phase}: {trace}");
    }
    assert!(trace.contains("elapsed_us="), "{trace}");
    assert!(!trace.contains("sensitive-"), "{trace}");
    assert!(!trace.contains(&daemon.store_url()), "{trace}");
    assert!(!trace.contains("NIX_CONFIG"), "{trace}");
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
        assert!(query.query_valid_paths(&[b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-sensitive-path-marker".to_vec()]).is_err());
    });
    assert!(trace.contains("store.query.spawn"), "{trace}");
    assert!(trace.contains("success=false"), "{trace}");
    assert!(!trace.contains("sensitive-"), "{trace}");
}
