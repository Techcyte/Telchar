//! Checks batch validity against real daemon registrations and unavailable endpoints.

use std::os::unix::net::UnixStream;
use telchar::fixture::nix::{NixFixture, TrustMode};
use telchar::store::query::{GatewayStoreQuery, QueryValidPathsStore};

#[test]
fn real_daemon_returns_present_paths_from_mixed_batch() {
    for trust in [TrustMode::Trusted, TrustMode::Untrusted] {
        let fixture = NixFixture::create().unwrap();
        let source = fixture.temp_dir().join("batch-input");
        std::fs::write(&source, "batch validity content").unwrap();
        let added = std::process::Command::new("nix-store")
            .args(["--store", "local", "--add"])
            .arg(&source)
            .envs(fixture.environment())
            .output()
            .unwrap();
        assert!(
            added.status.success(),
            "{}",
            String::from_utf8_lossy(&added.stderr)
        );
        let present = String::from_utf8(added.stdout)
            .unwrap()
            .trim()
            .as_bytes()
            .to_vec();
        let absent = format!(
            "{}/{}-absent",
            fixture.store_dir().display(),
            "a".repeat(32)
        )
        .into_bytes();
        let mut daemon = fixture.start_daemon(trust).unwrap();
        let stream =
            UnixStream::connect(daemon.store_url().strip_prefix("unix://").unwrap()).unwrap();
        let mut client = nix_worker_protocol::WorkerClient::connect_with_store_directory(
            stream,
            fixture.store_dir().as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        for substitute in [false, true] {
            assert_eq!(
                client
                    .query_valid_paths(&[present.clone(), absent.clone()], substitute)
                    .unwrap(),
                vec![present.clone()]
            );
            assert!(
                client
                    .query_valid_paths(std::slice::from_ref(&absent), substitute)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                client
                    .query_valid_paths(std::slice::from_ref(&present), substitute)
                    .unwrap(),
                vec![present.clone()]
            );
        }
        drop(client);
        daemon.stop().unwrap();
        fixture.cleanup().unwrap();
    }
}

#[test]
fn gateway_query_rejects_missing_endpoint_without_leaking_it() {
    let mut query = GatewayStoreQuery::new(
        telchar::store::GatewayStoreEndpoint::parse("unix:///nonexistent/sensitive-socket")
            .unwrap(),
    );
    let error = query
        .query_valid_paths(
            &[b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-input".to_vec()],
            false,
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "gateway Nix daemon connection failed");
}
