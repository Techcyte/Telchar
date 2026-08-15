//! Focused reconciliation contracts.

use super::*;

#[test]
fn reconciliation_removes_only_durable_released_roots() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "reconcile-request",
        "/nix/store/11111111111111111111111111111111-reconcile.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "reconcile-released",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "reconcile-request",
        "/nix/store/11111111111111111111111111111111-reconcile.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("released lease persists");
    telchar::persistence::release_unattached_request_leases(fixture.url(), "reconcile-request")
        .expect("lease releases");
    telchar::persistence::create_build_request(
        fixture.url(),
        "reconcile-active-request",
        "/nix/store/22222222222222222222222222222222-reconcile-active.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("active request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "reconcile-active",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "reconcile-active-request",
        "/nix/store/22222222222222222222222222222222-reconcile-active.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("active lease persists");
    let root_directory = std::env::temp_dir().join(format!(
        "telchar-retention-reconcile-{}",
        std::process::id()
    ));
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    std::os::unix::fs::symlink(
        "/nix/store/11111111111111111111111111111111-reconcile.drv",
        root_directory.join("reconcile-released"),
    )
    .expect("released root creates");
    std::os::unix::fs::symlink(
        "/nix/store/22222222222222222222222222222222-reconcile-active.drv",
        root_directory.join("reconcile-active"),
    )
    .expect("active root creates");
    let mut backend = NixStoreRetentionBackend::new("unix:///missing", &root_directory)
        .expect("retention backend configures");

    telchar::store::retention::reconcile_released_request_leases(fixture.url(), &mut backend)
        .expect("released roots reconcile");

    assert!(fs::symlink_metadata(root_directory.join("reconcile-released")).is_err());
    assert!(fs::symlink_metadata(root_directory.join("reconcile-active")).is_ok());
    fs::remove_dir_all(root_directory).expect("root directory cleans");
}

#[test]
fn expiry_pass_releases_due_output_and_preserves_future_output() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "expiry-retention-request",
        "/nix/store/11111111111111111111111111111111-expiry-retention.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let leases = telchar::persistence::create_request_output_leases(
        fixture.url(),
        "expiry-retention-request",
        Duration::from_secs(60),
        &[
            (
                "expiry-retention-due".to_owned(),
                "/nix/store/22222222222222222222222222222222-expiry-due".to_owned(),
            ),
            (
                "expiry-retention-future".to_owned(),
                "/nix/store/33333333333333333333333333333333-expiry-future".to_owned(),
            ),
        ],
    )
    .expect("output leases persist");
    let due = leases[0].expires_at.expect("due deadline exists");
    fixture
        .connect()
        .execute(
            "UPDATE store_leases SET expires_at = expires_at + interval '1 hour' WHERE lease_id = 'expiry-retention-future'",
            &[],
        )
        .expect("future deadline moves");
    let root_directory = std::env::temp_dir().join(format!(
        "telchar-output-expiry-reconcile-{}",
        std::process::id()
    ));
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    for (lease_id, store_path) in [
        (
            "expiry-retention-due",
            "/nix/store/22222222222222222222222222222222-expiry-due",
        ),
        (
            "expiry-retention-future",
            "/nix/store/33333333333333333333333333333333-expiry-future",
        ),
    ] {
        std::os::unix::fs::symlink(store_path, root_directory.join(lease_id))
            .expect("output root creates");
    }
    let mut backend = NixStoreRetentionBackend::new("unix:///missing", &root_directory)
        .expect("retention backend configures");

    telchar::store::retention::reconcile_output_retention(fixture.url(), &mut backend, due)
        .expect("output expiry reconciles");

    assert!(fs::symlink_metadata(root_directory.join("expiry-retention-due")).is_err());
    assert!(fs::symlink_metadata(root_directory.join("expiry-retention-future")).is_ok());
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "expiry-retention-due")
            .expect("due lease reads")
            .expect("due lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Released
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "expiry-retention-future")
            .expect("future lease reads")
            .expect("future lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
    fs::remove_dir_all(root_directory).expect("root directory cleans");
}

#[test]
fn committed_expiry_retries_root_removal_from_released_row() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "expiry-retry-request",
        "/nix/store/11111111111111111111111111111111-expiry-retry.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let lease = telchar::persistence::create_request_output_leases(
        fixture.url(),
        "expiry-retry-request",
        Duration::from_secs(60),
        &[(
            "expiry-retry-output".to_owned(),
            "/nix/store/22222222222222222222222222222222-expiry-retry".to_owned(),
        )],
    )
    .expect("output lease persists")
    .remove(0);
    let root_directory = std::env::temp_dir().join(format!(
        "telchar-output-expiry-retry-{}",
        std::process::id()
    ));
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    fs::write(root_directory.join("expiry-retry-output"), b"conflict")
        .expect("conflicting root creates");
    let mut backend = NixStoreRetentionBackend::new("unix:///missing", &root_directory)
        .expect("retention backend configures");
    let now = lease.expires_at.expect("deadline exists");

    assert!(telchar::store::retention::reconcile_output_retention(
        fixture.url(),
        &mut backend,
        now
    )
    .is_err());
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "expiry-retry-output")
            .expect("lease reads")
            .expect("lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Released
    );
    fs::remove_file(root_directory.join("expiry-retry-output")).expect("conflict removes");
    std::os::unix::fs::symlink(
        "/nix/store/22222222222222222222222222222222-expiry-retry",
        root_directory.join("expiry-retry-output"),
    )
    .expect("matching root creates");

    telchar::store::retention::reconcile_output_retention(
        fixture.url(),
        &mut backend,
        SystemTime::now(),
    )
    .expect("released row retries root removal");

    assert!(fs::symlink_metadata(root_directory.join("expiry-retry-output")).is_err());
    fs::remove_dir_all(root_directory).expect("root directory cleans");
}
