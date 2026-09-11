//! Focused reconciliation contracts.

use super::*;

#[test]
fn reconciliation_releases_leases_abandoned_by_closed_sessions() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "abandoned-session",
        requester,
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "abandoned-request",
        "/nix/store/11111111111111111111111111111111-abandoned.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "abandoned-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "abandoned-request",
        "/nix/store/11111111111111111111111111111111-abandoned.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "abandoned-input",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "abandoned-request",
        "/nix/store/22222222222222222222222222222222-abandoned-input",
        telchar::persistence::StoreLeasePurpose::Input,
    )
    .expect("input lease persists");
    telchar::persistence::attach_request(fixture.url(), "abandoned-session", "abandoned-request")
        .expect("request attaches");
    telchar::persistence::close_protocol_session(fixture.url(), "abandoned-session")
        .expect("session closes");
    let root_directory = std::env::temp_dir().join(format!(
        "telchar-retention-abandoned-{}",
        std::process::id()
    ));
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    for (lease_id, store_path) in [
        (
            "abandoned-derivation",
            "/nix/store/11111111111111111111111111111111-abandoned.drv",
        ),
        (
            "abandoned-input",
            "/nix/store/22222222222222222222222222222222-abandoned-input",
        ),
    ] {
        std::os::unix::fs::symlink(store_path, root_directory.join(lease_id))
            .expect("retention root creates");
    }
    let mut backend = NixStoreRetentionBackend::new("unix:///missing", &root_directory)
        .expect("retention backend configures");

    telchar::store::retention::reconcile_startup_retention(
        fixture.url(),
        &mut backend,
        SystemTime::now(),
    )
    .expect("abandoned request reconciles");

    for lease_id in ["abandoned-derivation", "abandoned-input"] {
        assert!(fs::symlink_metadata(root_directory.join(lease_id)).is_err());
        assert_eq!(
            telchar::persistence::read_store_lease(fixture.url(), lease_id)
                .expect("lease reads")
                .expect("lease exists")
                .state,
            telchar::persistence::StoreLeaseState::Reconciled
        );
    }
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "abandoned-session",
            "abandoned-request",
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Detached
    );

    fs::remove_dir_all(root_directory).expect("root directory cleans");
}

#[test]
fn reconciliation_releases_leases_abandoned_by_interrupted_sessions() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "interrupted-session",
        requester,
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "interrupted-request",
        "/nix/store/11111111111111111111111111111111-interrupted.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "interrupted-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "interrupted-request",
        "/nix/store/11111111111111111111111111111111-interrupted.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");
    telchar::persistence::attach_request(
        fixture.url(),
        "interrupted-session",
        "interrupted-request",
    )
    .expect("request attaches");
    let root_directory = std::env::temp_dir().join(format!(
        "telchar-retention-interrupted-{}",
        std::process::id()
    ));
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    std::os::unix::fs::symlink(
        "/nix/store/11111111111111111111111111111111-interrupted.drv",
        root_directory.join("interrupted-derivation"),
    )
    .expect("retention root creates");
    let mut backend = NixStoreRetentionBackend::new("unix:///missing", &root_directory)
        .expect("retention backend configures");

    telchar::store::retention::reconcile_startup_retention(
        fixture.url(),
        &mut backend,
        SystemTime::now(),
    )
    .expect("interrupted request reconciles");

    assert!(fs::symlink_metadata(root_directory.join("interrupted-derivation")).is_err());
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.url(), "interrupted-session")
            .expect("session reads")
            .expect("session exists")
            .state,
        telchar::persistence::ProtocolSessionState::Closed
    );
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "interrupted-session",
            "interrupted-request",
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Detached
    );

    fs::remove_dir_all(root_directory).expect("root directory cleans");
}

#[test]
fn reconciliation_releases_multiple_abandoned_batches() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    for suffix in ["first", "second"] {
        let session_id = format!("batch-{suffix}-session");
        let request_id = format!("batch-{suffix}-request");
        let lease_id = format!("batch-{suffix}-lease");
        let store_path = format!("/nix/store/11111111111111111111111111111111-{suffix}.drv");
        telchar::persistence::open_protocol_session(
            fixture.url(),
            &session_id,
            "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e",
            "ssh-pubkey:SHA256:test",
            "test-audit",
            "test-quota",
        )
        .expect("session opens");
        telchar::persistence::create_build_request(
            fixture.url(),
            &request_id,
            &store_path,
            "x86_64-linux",
            "test-audit",
            "test-quota",
        )
        .expect("request persists");
        telchar::persistence::create_store_lease(
            fixture.url(),
            &lease_id,
            telchar::persistence::StoreLeaseOwnerKind::Request,
            &request_id,
            &store_path,
            telchar::persistence::StoreLeasePurpose::Derivation,
        )
        .expect("lease persists");
        telchar::persistence::attach_request(fixture.url(), &session_id, &request_id)
            .expect("request attaches");
        telchar::persistence::close_protocol_session(fixture.url(), &session_id)
            .expect("session closes");
    }

    let first = telchar::persistence::release_abandoned_request_leases(fixture.url(), 1)
        .expect("first batch releases");
    let second = telchar::persistence::release_abandoned_request_leases(fixture.url(), 1)
        .expect("second batch releases");
    let third = telchar::persistence::release_abandoned_request_leases(fixture.url(), 1)
        .expect("empty batch succeeds");

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert!(third.is_empty());
}

#[test]
fn reconciliation_releases_abandoned_requests_without_derivation_leases() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "input-only-session",
        "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e",
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "input-only-request",
        "/nix/store/11111111111111111111111111111111-input-only.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "input-only-lease",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "input-only-request",
        "/nix/store/22222222222222222222222222222222-input-only",
        telchar::persistence::StoreLeasePurpose::Input,
    )
    .expect("input lease persists");
    telchar::persistence::attach_request(fixture.url(), "input-only-session", "input-only-request")
        .expect("request attaches");
    telchar::persistence::close_protocol_session(fixture.url(), "input-only-session")
        .expect("session closes");

    let released = telchar::persistence::release_abandoned_request_leases(fixture.url(), 16)
        .expect("input-only request releases");

    assert_eq!(released.len(), 1);
    assert_eq!(
        released[0].purpose,
        telchar::persistence::StoreLeasePurpose::Input
    );
}

#[test]
fn reconciliation_ignores_reconciled_output_leases() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "completed-output-session",
        "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e",
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "completed-output-request",
        "/nix/store/11111111111111111111111111111111-completed-output.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    for (lease_id, store_path, purpose) in [
        (
            "completed-output-input",
            "/nix/store/22222222222222222222222222222222-completed-output-input",
            telchar::persistence::StoreLeasePurpose::Input,
        ),
        (
            "completed-output-result",
            "/nix/store/33333333333333333333333333333333-completed-output-result",
            telchar::persistence::StoreLeasePurpose::Output,
        ),
    ] {
        telchar::persistence::create_store_lease(
            fixture.url(),
            lease_id,
            telchar::persistence::StoreLeaseOwnerKind::Request,
            "completed-output-request",
            store_path,
            purpose,
        )
        .expect("lease persists");
    }
    telchar::persistence::release_store_lease(fixture.url(), "completed-output-result")
        .expect("output releases");
    telchar::persistence::reconcile_store_leases(
        fixture.url(),
        &["completed-output-result".to_owned()],
    )
    .expect("output reconciles");
    telchar::persistence::attach_request(
        fixture.url(),
        "completed-output-session",
        "completed-output-request",
    )
    .expect("request attaches");
    telchar::persistence::close_protocol_session(fixture.url(), "completed-output-session")
        .expect("session closes");

    let released = telchar::persistence::release_abandoned_request_leases(fixture.url(), 16)
        .expect("active input releases beside reconciled output");

    assert_eq!(released.len(), 1);
    assert_eq!(
        released[0].purpose,
        telchar::persistence::StoreLeasePurpose::Input
    );
}

#[test]
fn reconciliation_resumes_after_released_roots_were_removed() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let root_directory =
        std::env::temp_dir().join(format!("telchar-retention-resume-{}", std::process::id()));
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    let mut backend = NixStoreRetentionBackend::new("unix:///missing", &root_directory)
        .expect("retention backend configures");
    for suffix in ["released", "active"] {
        let session_id = format!("resume-{suffix}-session");
        let request_id = format!("resume-{suffix}-request");
        let lease_id = format!("resume-{suffix}-lease");
        let store_path = format!("/nix/store/11111111111111111111111111111111-{suffix}.drv");
        telchar::persistence::open_protocol_session(
            fixture.url(),
            &session_id,
            "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e",
            "ssh-pubkey:SHA256:test",
            "test-audit",
            "test-quota",
        )
        .expect("session opens");
        telchar::persistence::create_build_request(
            fixture.url(),
            &request_id,
            &store_path,
            "x86_64-linux",
            "test-audit",
            "test-quota",
        )
        .expect("request persists");
        telchar::persistence::create_store_lease(
            fixture.url(),
            &lease_id,
            telchar::persistence::StoreLeaseOwnerKind::Request,
            &request_id,
            &store_path,
            telchar::persistence::StoreLeasePurpose::Derivation,
        )
        .expect("lease persists");
        telchar::persistence::attach_request(fixture.url(), &session_id, &request_id)
            .expect("request attaches");
        telchar::persistence::close_protocol_session(fixture.url(), &session_id)
            .expect("session closes");
        std::os::unix::fs::symlink(&store_path, root_directory.join(&lease_id))
            .expect("retention root creates");
    }
    let released = telchar::persistence::release_abandoned_request_leases(fixture.url(), 1)
        .expect("first request releases");
    assert_eq!(released.len(), 1);
    backend
        .release(
            &released
                .iter()
                .map(|lease| ReleasedRetentionEntry::new(&lease.lease_id, &lease.store_path))
                .collect::<Vec<_>>(),
        )
        .expect("released root removes");

    telchar::store::retention::reconcile_startup_retention(
        fixture.url(),
        &mut backend,
        SystemTime::now(),
    )
    .expect("partial cleanup resumes");

    assert!(
        fs::read_dir(&root_directory)
            .expect("root directory reads")
            .next()
            .is_none()
    );
    let second = telchar::persistence::release_abandoned_request_leases(fixture.url(), 1)
        .expect("no abandoned request remains");
    assert!(second.is_empty());

    fs::remove_dir_all(root_directory).expect("root directory cleans");
}

#[test]
fn reconciliation_preserves_attached_requests_for_open_sessions() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "active-session",
        requester,
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "active-request",
        "/nix/store/11111111111111111111111111111111-active.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "active-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "active-request",
        "/nix/store/11111111111111111111111111111111-active.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");
    telchar::persistence::attach_request(fixture.url(), "active-session", "active-request")
        .expect("request attaches");
    let root_directory =
        std::env::temp_dir().join(format!("telchar-retention-active-{}", std::process::id()));
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    std::os::unix::fs::symlink(
        "/nix/store/11111111111111111111111111111111-active.drv",
        root_directory.join("active-derivation"),
    )
    .expect("retention root creates");
    let mut backend = NixStoreRetentionBackend::new("unix:///missing", &root_directory)
        .expect("retention backend configures");

    telchar::store::retention::reconcile_released_request_leases(fixture.url(), &mut backend)
        .expect("active request reconciliation succeeds");

    assert!(fs::symlink_metadata(root_directory.join("active-derivation")).is_ok());
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "active-derivation")
            .expect("lease reads")
            .expect("lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "active-session",
            "active-request",
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Attached
    );

    fs::remove_dir_all(root_directory).expect("root directory cleans");
}

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
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "reconcile-released")
            .expect("released lease reads")
            .expect("released lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Reconciled
    );
    assert!(
        telchar::persistence::read_released_request_leases_page(fixture.url(), None, 256)
            .expect("released retry page reads")
            .is_empty()
    );

    telchar::store::retention::reconcile_released_request_leases(fixture.url(), &mut backend)
        .expect("drained reconciliation is idempotent");

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
        telchar::persistence::StoreLeaseState::Reconciled
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

    assert!(
        telchar::store::retention::reconcile_output_retention(fixture.url(), &mut backend, now)
            .is_err()
    );
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
