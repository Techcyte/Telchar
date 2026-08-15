//! Focused release contracts.

use super::*;

#[test]
fn empty_retention_set_does_not_connect_to_daemon() {
    let root = std::env::temp_dir().join(format!("telchar-retention-empty-{}", std::process::id()));
    fs::create_dir_all(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let mut backend =
        NixStoreRetentionBackend::new("unix:///missing", &root).expect("backend configures");

    assert!(backend
        .retain(&[])
        .expect("empty retain succeeds")
        .is_empty());
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

    assert!(!daemon
        .is_valid_path(&released)
        .expect("released path validity reads"));
    assert!(daemon
        .is_valid_path(&active)
        .expect("active path validity reads"));
    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

#[test]
fn release_accepts_distinct_leases_for_the_same_store_path() {
    let root_directory = std::env::temp_dir().join(format!(
        "telchar-retention-shared-release-{}",
        std::process::id()
    ));
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    let store_path = "/nix/store/11111111111111111111111111111111-shared-release";
    for lease_id in ["first-release", "second-release"] {
        std::os::unix::fs::symlink(store_path, root_directory.join(lease_id))
            .expect("root creates");
    }
    let mut backend = NixStoreRetentionBackend::new("unix:///missing", &root_directory)
        .expect("retention backend configures");

    backend
        .release(&[
            ReleasedRetentionEntry::new("first-release", store_path),
            ReleasedRetentionEntry::new("second-release", store_path),
        ])
        .expect("shared store path roots release");

    assert!(fs::read_dir(&root_directory)
        .expect("root directory reads")
        .next()
        .is_none());
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

    assert!(backend
        .release(&[ReleasedRetentionEntry::new(
            "release-conflict",
            released.to_string_lossy(),
        )])
        .is_err());
    assert_eq!(fs::read_link(&root).expect("conflict persists"), other);
    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}
