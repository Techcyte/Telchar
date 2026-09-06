//! Checks scoped export stream ownership against an isolated real daemon.

use super::StoreExportBackend;
use crate::fixture::nix::{NixFixture, TrustMode};
use std::net::Shutdown;
use std::path::Path;

#[test]
fn failed_export_operation_discards_stream_without_replay() {
    for _ in 0..20 {
        let fixture = NixFixture::create().expect("fixture creates");
        let mut daemon = fixture
            .start_daemon(TrustMode::Trusted)
            .expect("real daemon starts");
        let mut backend = daemon.export_backend().expect("exporter creates");
        backend
            .with_connection(|connection| connection.query_missing(&[]))
            .expect("real query completes");
        let handle = backend
            .connection
            .as_ref()
            .unwrap()
            .shutdown_handle()
            .unwrap();
        let mut attempts = 0;
        let error = backend
            .with_connection(|connection| {
                attempts += 1;
                handle.shutdown(Shutdown::Both)?;
                connection.query_missing(&[])
            })
            .expect_err("closed socket fails");
        assert_eq!(error.to_string(), "gateway Nix daemon connection failed");
        assert_eq!(attempts, 1, "failed operation is not replayed");
        assert!(backend.connection.is_none(), "failed stream is discarded");
        backend
            .with_connection(|connection| connection.query_missing(&[]))
            .expect("next operation reconnects");
        assert!(backend.connection.is_some());
        backend
            .query_path_info(Path::new(
                "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-absent",
            ))
            .expect_err("unregistered path fails metadata query");
        assert!(
            backend.connection.is_none(),
            "metadata failure discards stream"
        );
        backend
            .with_connection(|connection| connection.query_missing(&[]))
            .expect("query after metadata failure reconnects");
        drop(handle);
        drop(backend);
        daemon.stop().expect("daemon stops");
        fixture.cleanup().expect("fixture cleans");
    }
}

#[test]
#[ignore = "requires TELCHAR_EXPORT_TEST_STORE and TELCHAR_EXPORT_TEST_PATH for a real canonical store"]
fn verified_export_failure_discards_connection() {
    use super::{export_verified_nar, GatewayStoreEndpoint, GatewayStoreExportBackend};
    use std::io::Read;

    let endpoint =
        GatewayStoreEndpoint::parse(&std::env::var("TELCHAR_EXPORT_TEST_STORE").unwrap()).unwrap();
    let path = std::path::PathBuf::from(std::env::var("TELCHAR_EXPORT_TEST_PATH").unwrap());
    let mut backend = GatewayStoreExportBackend::new(endpoint);
    let mut bytes = Vec::new();
    let first = export_verified_nar(&path, &mut bytes, &mut backend).unwrap();
    let mut handle = backend
        .connection
        .as_ref()
        .unwrap()
        .shutdown_handle()
        .unwrap();
    let second = export_verified_nar(&path, &mut std::io::sink(), &mut backend).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.nar_size, bytes.len() as u64);
    let mut full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let error = export_verified_nar(&path, &mut full, &mut backend)
        .expect_err("real destination write failure propagates");
    assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
    assert!(backend.connection.is_none());
    assert_eq!(
        handle.read(&mut [0]).unwrap(),
        0,
        "discard shuts cloned handles"
    );
    let recovered = export_verified_nar(&path, &mut std::io::sink(), &mut backend).unwrap();
    assert_eq!(first, recovered);
    assert!(backend.connection.is_some());
    let contents = super::load_stored_derivation(&path, 1024, &mut backend).unwrap();
    assert!(!contents.is_empty());
    let error = super::load_stored_derivation(&path, 0, &mut backend)
        .expect_err("bounded derivation read fails");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(backend.connection.is_none());
    assert_eq!(
        super::load_stored_derivation(&path, 1024, &mut backend).unwrap(),
        contents
    );

    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o644);
    std::fs::set_permissions(&path, permissions).unwrap();
    let mut corrupted = contents.clone();
    corrupted[0] ^= 1;
    std::fs::write(&path, &corrupted).unwrap();
    let error = export_verified_nar(&path, &mut std::io::sink(), &mut backend)
        .expect_err("real daemon bytes must match registered hash");
    assert_eq!(error.to_string(), "exported NAR hash mismatch");
    assert!(
        backend.connection.is_none(),
        "post-transfer verification failure discards stream"
    );
    let error = super::load_stored_derivation(&path, 1024, &mut backend)
        .expect_err("derivation hash verification fails");
    assert_eq!(error.to_string(), "exported derivation NAR hash mismatch");
    assert!(backend.connection.is_none());
    std::fs::write(&path, &contents).unwrap();
    assert_eq!(
        export_verified_nar(&path, &mut std::io::sink(), &mut backend).unwrap(),
        first
    );
    backend.discard_connection();
    assert!(backend.connection.is_none());
}
