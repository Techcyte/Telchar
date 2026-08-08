use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use telchar::nix_fixture::{NixDaemon, NixFixture, TrustMode};
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
    let mut backend = NixStoreRetentionBackend::new(daemon.store_url(), &root_directory)
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
    let mut backend = NixStoreRetentionBackend::new(daemon.store_url(), &root_directory)
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

    let mut backend = NixStoreRetentionBackend::new(daemon.store_url(), &root_directory)
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
    let mut backend = NixStoreRetentionBackend::new(store_uri, root_directory)?;
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
