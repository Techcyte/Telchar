//! Tests retry classification without weakening root identity checks.

use super::*;

#[test]
fn cleanup_permission_failure_preserves_root_for_retry() {
    let root = tempfile::tempdir().unwrap();
    let roots = root.path().join("roots");
    fs::create_dir(&roots).unwrap();
    fs::set_permissions(&roots, fs::Permissions::from_mode(0o700)).unwrap();
    let path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-output";
    std::os::unix::fs::symlink(path, roots.join("released")).unwrap();
    let mut backend = NixStoreRetentionBackend::new("unix:///missing", &roots).unwrap();
    fs::set_permissions(&roots, fs::Permissions::from_mode(0o500)).unwrap();
    let result = backend.release(&[ReleasedRetentionEntry::new("released", path)]);
    let preparation = NixStoreRetentionBackend::new("unix:///missing", &roots);
    fs::set_permissions(&roots, fs::Permissions::from_mode(0o700)).unwrap();
    let error = result.expect_err("read-only directory prevents root removal");
    assert!(telchar::store::retention::cleanup_retryable(&error));
    assert!(telchar::store::retention::cleanup_retryable(
        &preparation
            .err()
            .expect("read-only root directory rejects backend preparation")
    ));
    assert_eq!(
        fs::read_link(roots.join("released")).unwrap(),
        std::path::Path::new(path)
    );
    backend
        .release(&[ReleasedRetentionEntry::new("released", path)])
        .unwrap();
    assert!(!roots.join("released").exists());
    std::os::unix::fs::symlink(
        "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-foreign",
        roots.join("released"),
    )
    .unwrap();
    let error = backend
        .release(&[ReleasedRetentionEntry::new("released", path)])
        .unwrap_err();
    assert!(!telchar::store::retention::cleanup_retryable(&error));
}

#[test]
fn maintenance_retries_root_cleanup_without_losing_reconciliation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    fixture.connect().batch_execute("INSERT INTO build_requests (request_id,derivation_path,system) VALUES ('cleanup-request','/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-cleanup.drv','x86_64-linux'); INSERT INTO store_leases (lease_id,owner_kind,owner_id,store_path,purpose,state,nar_size,created_at,released_at) VALUES ('released','request','cleanup-request','/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-output','input','released',1,now(),now())").unwrap();
    let owner = std::sync::Arc::new(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(20),
        )
        .unwrap(),
    );
    let root = tempfile::tempdir().unwrap();
    let roots = root.path().join("roots");
    fs::create_dir(&roots).unwrap();
    fs::set_permissions(&roots, fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(
        "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-output",
        roots.join("released"),
    )
    .unwrap();
    let mut backend = NixStoreRetentionBackend::new("unix:///missing", &roots).unwrap();
    fs::set_permissions(&roots, fs::Permissions::from_mode(0o500)).unwrap();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = attempts.clone();
    let database = owner.database().clone();
    let mut service = telchar::service::daemon_services::MaintenanceService::start_with_ownership(
        Duration::from_millis(20),
        owner,
        move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            telchar::store::retention::reconcile_released_request_leases(&database, &mut backend)
        },
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while attempts.load(std::sync::atomic::Ordering::SeqCst) < 2 {
        service.check().unwrap();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "released")
            .unwrap()
            .unwrap()
            .state,
        telchar::persistence::StoreLeaseState::Released
    );
    assert!(fs::symlink_metadata(roots.join("released")).is_ok());
    fs::set_permissions(&roots, fs::Permissions::from_mode(0o700)).unwrap();
    loop {
        service.check().unwrap();
        if telchar::persistence::read_store_lease(fixture.url(), "released")
            .unwrap()
            .unwrap()
            .state
            == telchar::persistence::StoreLeaseState::Reconciled
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    service.shutdown().unwrap();
    assert!(fs::symlink_metadata(roots.join("released")).is_err());
}
