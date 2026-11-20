//! Focused durability contracts.

use super::*;

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
fn pending_hook_delivery_survives_gc_and_releases_after_acknowledgment() {
    use telchar::{backend::BackendKind, persistence};
    let fixture = NixFixture::create().unwrap();
    let mut daemon = fixture.start_daemon(TrustMode::Trusted).unwrap();
    let output = build_fixture_path(&fixture, &daemon, "hook_delivery-retained", "published");
    let control = build_fixture_path(&fixture, &daemon, "hook_delivery-control", "control");
    let root_directory = fixture.root().join("hook_delivery-roots");
    fs::create_dir(&root_directory).unwrap();
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700)).unwrap();
    let database_fixture = PostgresFixture::start();
    persistence::migrate(database_fixture.url()).unwrap();
    let database = persistence::Database::connect(database_fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hook_delivery-retention.drv";
    persistence::claim_shared_build(
        &database,
        drv,
        &[1; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &[output.to_str().unwrap()],
    )
    .unwrap();
    persistence::start_shared_build(&database, drv).unwrap();
    persistence::collect_shared_build(&database, drv).unwrap();
    let mut retention = NixStoreRetentionBackend::new_with_store_directory(
        daemon.store_url(),
        daemon.store_dir(),
        &root_directory,
    )
    .unwrap();
    telchar::service::hook_delivery::prepare_outputs(
        &database,
        drv,
        &mut retention,
        &[output.to_str().unwrap().to_owned()],
    )
    .unwrap();
    persistence::complete_shared_build_success(
        &database,
        drv,
        &serde_json::json!({}),
        Duration::from_secs(1),
    )
    .unwrap();
    daemon.collect_garbage().unwrap();
    assert!(daemon.is_valid_path(&output).unwrap());
    assert!(!daemon.is_valid_path(&control).unwrap());
    daemon.stop().unwrap();
    let mut restarted = fixture.start_daemon(TrustMode::Trusted).unwrap();
    let mut retention = NixStoreRetentionBackend::new_with_store_directory(
        restarted.store_url(),
        restarted.store_dir(),
        &root_directory,
    )
    .unwrap();
    telchar::service::hook_delivery::reconcile_pending_roots(&database, &mut retention, None)
        .unwrap();
    restarted.collect_garbage().unwrap();
    assert!(restarted.is_valid_path(&output).unwrap());
    let hook_delivery =
        persistence::claim_post_build_hooks(&database, "published", Duration::from_secs(30))
            .unwrap()
            .unwrap();
    assert!(persistence::acknowledge_post_build_hooks(&database, &hook_delivery).unwrap());
    telchar::service::hook_delivery::reconcile_released_roots(&database, &mut retention).unwrap();
    restarted.collect_garbage().unwrap();
    assert!(!restarted.is_valid_path(&output).unwrap());
    restarted.stop().unwrap();
    fixture.cleanup().unwrap();
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
