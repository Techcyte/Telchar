//! Tests store retention contracts and failure boundaries, including empty retention set does not connect to daemon.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, SystemTime};

use telchar::nix_fixture::{NixDaemon, NixFixture, TrustMode};

mod support;

use support::postgres::PostgresFixture;
use telchar::store_retention::{
    NixStoreRetentionBackend, ReleasedRetentionEntry, RetentionEntry, StoreRetentionBackend,
};

#[test]
fn empty_retention_set_does_not_connect_to_daemon() {
    let root = std::env::temp_dir().join(format!("telchar-retention-empty-{}", std::process::id()));
    fs::create_dir_all(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let mut backend =
        NixStoreRetentionBackend::new("unix:///missing", &root).expect("backend configures");

    assert!(
        backend
            .retain(&[])
            .expect("empty retain succeeds")
            .is_empty()
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn release_removes_only_matching_durable_root() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let released = build_fixture_path(&fixture, &daemon, "released-root", "released");
    let active = build_fixture_path(&fixture, &daemon, "active-root", "active");
    let root_directory = fixture.root().join("gc-roots");
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    let mut backend = NixStoreRetentionBackend::new_with_store_directory(
        daemon.store_url(),
        daemon.store_dir(),
        &root_directory,
    )
    .expect("retention backend configures");
    backend
        .retain(&[RetentionEntry::new(
            "released-root",
            released.to_string_lossy(),
        )])
        .expect("released root retains");
    backend
        .retain(&[RetentionEntry::new("active-root", active.to_string_lossy())])
        .expect("active root retains");

    backend
        .release(&[ReleasedRetentionEntry::new(
            "released-root",
            released.to_string_lossy(),
        )])
        .expect("released root removes");

    assert!(!root_directory.join("released-root").exists());
    assert_eq!(
        fs::read_link(root_directory.join("active-root")).expect("active root remains"),
        active
    );
    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

#[test]
fn released_root_is_collectable_while_active_root_survives_private_gc() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let released = build_fixture_path(&fixture, &daemon, "collectable-root", "released");
    let active = build_fixture_path(&fixture, &daemon, "surviving-root", "active");
    let root_directory = fixture.root().join("gc-roots");
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    let mut backend = NixStoreRetentionBackend::new_with_store_directory(
        daemon.store_url(),
        daemon.store_dir(),
        &root_directory,
    )
    .expect("retention backend configures");
    backend
        .retain(&[RetentionEntry::new(
            "collectable-root",
            released.to_string_lossy(),
        )])
        .expect("released root retains");
    backend
        .retain(&[RetentionEntry::new(
            "surviving-root",
            active.to_string_lossy(),
        )])
        .expect("active root retains");

    backend
        .release(&[ReleasedRetentionEntry::new(
            "collectable-root",
            released.to_string_lossy(),
        )])
        .expect("released root removes");
    daemon.collect_garbage().expect("private GC succeeds");

    assert!(
        !daemon
            .is_valid_path(&released)
            .expect("released path validity reads")
    );
    assert!(
        daemon
            .is_valid_path(&active)
            .expect("active path validity reads")
    );
    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
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

    telchar::store_retention::reconcile_released_request_leases(fixture.url(), &mut backend)
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

    telchar::store_retention::reconcile_output_retention(fixture.url(), &mut backend, due)
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

    assert!(
        telchar::store_retention::reconcile_output_retention(fixture.url(), &mut backend, now)
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

    telchar::store_retention::reconcile_output_retention(
        fixture.url(),
        &mut backend,
        SystemTime::now(),
    )
    .expect("released row retries root removal");

    assert!(fs::symlink_metadata(root_directory.join("expiry-retry-output")).is_err());
    fs::remove_dir_all(root_directory).expect("root directory cleans");
}

#[test]
fn release_rejects_invalid_or_duplicate_entries_without_mutation() {
    let root_directory = std::env::temp_dir().join(format!(
        "telchar-retention-invalid-release-{}",
        std::process::id()
    ));
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    let root = root_directory.join("valid-release");
    std::os::unix::fs::symlink(
        "/nix/store/11111111111111111111111111111111-valid-release",
        &root,
    )
    .expect("root creates");
    let mut backend = NixStoreRetentionBackend::new("unix:///missing", &root_directory)
        .expect("retention backend configures");

    for entries in [
        vec![ReleasedRetentionEntry::new(
            "nested/release",
            "/nix/store/11111111111111111111111111111111-valid-release",
        )],
        vec![
            ReleasedRetentionEntry::new(
                "valid-release",
                "/nix/store/11111111111111111111111111111111-valid-release",
            ),
            ReleasedRetentionEntry::new(
                "valid-release",
                "/nix/store/22222222222222222222222222222222-other-release",
            ),
        ],
    ] {
        assert!(backend.release(&entries).is_err());
        assert!(fs::symlink_metadata(&root).is_ok());
    }
    fs::remove_dir_all(root_directory).expect("root directory cleans");
}

#[test]
fn release_rejects_conflicts_without_removing_any_root() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let released = build_fixture_path(&fixture, &daemon, "release-conflict", "released");
    let other = build_fixture_path(&fixture, &daemon, "release-conflict-other", "other");
    let root_directory = fixture.root().join("gc-roots");
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    let mut backend = NixStoreRetentionBackend::new_with_store_directory(
        daemon.store_url(),
        daemon.store_dir(),
        &root_directory,
    )
    .expect("retention backend configures");
    backend
        .retain(&[RetentionEntry::new(
            "release-conflict",
            released.to_string_lossy(),
        )])
        .expect("root retains");
    let root = root_directory.join("release-conflict");
    fs::remove_file(&root).expect("root removes");
    std::os::unix::fs::symlink(&other, &root).expect("conflicting root creates");

    assert!(
        backend
            .release(&[ReleasedRetentionEntry::new(
                "release-conflict",
                released.to_string_lossy(),
            )])
            .is_err()
    );
    assert_eq!(fs::read_link(&root).expect("conflict persists"), other);
    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

#[test]
fn existing_exact_root_is_idempotent_but_conflicts_do_not_clobber() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let leased = build_fixture_path(&fixture, &daemon, "idempotent-leased", "leased");
    let other = build_fixture_path(&fixture, &daemon, "idempotent-other", "other");
    let root_directory = fixture.root().join("gc-roots");
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    let root = root_directory.join("lease-idempotent");

    assert!(
        retain_fixture_path(
            &fixture,
            &daemon.store_url(),
            &root_directory,
            "lease-idempotent",
            &leased
        )
        .is_ok(),
        "initial retain fails"
    );
    assert_eq!(fs::read_link(&root).expect("root target reads"), leased);
    assert!(
        retain_fixture_path(
            &fixture,
            &daemon.store_url(),
            &root_directory,
            "lease-idempotent",
            &leased
        )
        .is_ok(),
        "same root retain is not idempotent"
    );
    assert_eq!(
        fs::read_link(&root).expect("idempotent root remains"),
        leased
    );

    fs::remove_file(&root).expect("root removes for conflict test");
    std::os::unix::fs::symlink(&other, &root).expect("conflicting symlink creates");
    let response = retain_fixture_path(
        &fixture,
        &daemon.store_url(),
        &root_directory,
        "lease-idempotent",
        &leased,
    );
    assert!(response.is_err(), "conflicting root succeeds");
    assert_eq!(
        fs::read_link(&root).expect("conflicting target remains"),
        other
    );

    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

#[test]
fn permanent_root_survives_daemon_restart_and_second_private_gc() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let leased = build_fixture_path(&fixture, &daemon, "restart-leased", "restart");
    let root_directory = fixture.root().join("gc-roots");
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    let root = root_directory.join("lease-restart");

    assert!(
        retain_fixture_path(
            &fixture,
            &daemon.store_url(),
            &root_directory,
            "lease-restart",
            &leased
        )
        .is_ok(),
        "initial retain fails"
    );
    daemon.collect_garbage().expect("first private GC succeeds");
    assert!(
        daemon
            .is_valid_path(&leased)
            .expect("first GC preserves lease")
    );
    daemon.stop().expect("fixture daemon stops");

    let mut restarted = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon restarts");
    assert_eq!(fs::read_link(&root).expect("root survives restart"), leased);
    restarted
        .collect_garbage()
        .expect("second private GC succeeds");
    assert!(
        restarted
            .is_valid_path(&leased)
            .expect("second GC preserves lease")
    );
    assert_eq!(fs::read(&leased).expect("leased content reads"), b"restart");

    restarted.stop().expect("restarted daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

#[test]
fn real_permanent_root_preserves_leased_path_while_gc_collects_unrooted_control() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let leased = build_fixture_path(&fixture, &daemon, "retained-leased", "leased");
    let control = build_fixture_path(&fixture, &daemon, "retained-control", "control");
    let root_directory = fixture.root().join("gc-roots");
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");

    assert!(
        daemon
            .is_valid_path(&leased)
            .expect("leased path valid before GC")
    );
    assert!(
        daemon
            .is_valid_path(&control)
            .expect("control path valid before GC")
    );

    let mut backend = NixStoreRetentionBackend::new_with_store_directory(
        daemon.store_url(),
        daemon.store_dir(),
        &root_directory,
    )
    .expect("retention backend configures");
    let retained = backend
        .retain(&[RetentionEntry::new(
            "lease-retained-fixture",
            leased.to_string_lossy(),
        )])
        .expect("leased path retains");
    assert_eq!(retained.len(), 1);
    let root = root_directory.join("lease-retained-fixture");
    assert_eq!(fs::read_link(&root).expect("root symlink reads"), leased);

    daemon
        .collect_garbage()
        .expect("private store garbage collects");
    assert!(
        daemon
            .is_valid_path(&leased)
            .expect("leased path valid after GC")
    );
    assert_eq!(fs::read(&leased).expect("leased path reads"), b"leased");
    assert!(
        !daemon
            .is_valid_path(&control)
            .expect("control path validity after GC"),
        "unrooted control path survived private-store GC: {control:?}"
    );

    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

fn retain_fixture_path(
    _fixture: &NixFixture,
    store_uri: &str,
    root_directory: &std::path::Path,
    lease_id: &str,
    store_path: &std::path::Path,
) -> std::io::Result<()> {
    let mut backend = NixStoreRetentionBackend::new_with_store_directory(
        store_uri,
        store_path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "store path has no directory",
            )
        })?,
        root_directory,
    )?;
    backend
        .retain(&[RetentionEntry::new(lease_id, store_path.to_string_lossy())])
        .map(|_| ())
}

fn build_fixture_path(
    fixture: &NixFixture,
    daemon: &NixDaemon,
    name: &str,
    contents: &str,
) -> std::path::PathBuf {
    let expression = format!(
        "derivation {{ name = \"{name}\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf {contents} > \\\"$out\\\"\" ]; }}"
    );
    let output = Command::new("nix")
        .envs(fixture.environment())
        .args([
            "--store",
            &daemon.store_url(),
            "build",
            "--impure",
            "--expr",
            &expression,
            "--no-link",
            "--print-out-paths",
        ])
        .output()
        .expect("fixture derivation builds");
    assert!(
        output.status.success(),
        "fixture derivation failed: {output:?}"
    );
    std::path::PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("output path is UTF-8")
            .trim(),
    )
}
