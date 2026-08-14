//! Tests IPC readiness.

use super::*;

#[test]
fn daemon_reconciles_expired_output_before_readiness() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    let gc_roots = root.join("gc-roots");
    fs::create_dir(&gc_roots).expect("GC root directory creates");
    fs::set_permissions(&gc_roots, fs::Permissions::from_mode(0o700))
        .expect("GC root directory permissions set");
    let database = PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        database.url(),
        "startup-expiry-request",
        "/nix/store/11111111111111111111111111111111-startup-expiry.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let lease = telchar::persistence::create_request_output_leases(
        database.url(),
        "startup-expiry-request",
        Duration::from_secs(60),
        &[(
            "startup-expiry-output".to_owned(),
            "/nix/store/22222222222222222222222222222222-startup-expiry".to_owned(),
        )],
    )
    .expect("output lease persists")
    .remove(0);
    database
        .connect()
        .execute(
            "UPDATE store_leases SET created_at = transaction_timestamp() - interval '2 minutes', expires_at = transaction_timestamp() - interval '1 minute' WHERE lease_id = 'startup-expiry-output'",
            &[],
        )
        .expect("output deadline expires");
    std::os::unix::fs::symlink(
        "/nix/store/22222222222222222222222222222222-startup-expiry",
        gc_roots.join(&lease.lease_id),
    )
    .expect("output root creates");

    let mut daemon = daemon_command(&socket, 1_000, true, database.url())
        .env("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY", &gc_roots)
        .env("TELCHAR_TEST_STORE_RETENTION", "1")
        .spawn()
        .expect("daemon starts");
    wait_for_socket(&socket, &mut daemon);

    assert!(fs::symlink_metadata(gc_roots.join(&lease.lease_id)).is_err());
    assert_eq!(
        telchar::persistence::read_store_lease(database.url(), &lease.lease_id)
            .expect("lease reads")
            .expect("lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Released
    );
    daemon.kill().expect("daemon stops");
    let _ = daemon.wait();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn daemon_refuses_readiness_when_output_reconciliation_fails() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    let gc_roots = root.join("gc-roots");
    fs::create_dir(&gc_roots).expect("GC root directory creates");
    fs::set_permissions(&gc_roots, fs::Permissions::from_mode(0o700))
        .expect("GC root directory permissions set");
    let database = PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        database.url(),
        "startup-conflict-request",
        "/nix/store/11111111111111111111111111111111-startup-conflict.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_request_output_leases(
        database.url(),
        "startup-conflict-request",
        Duration::from_secs(60),
        &[(
            "startup-conflict-output".to_owned(),
            "/nix/store/22222222222222222222222222222222-startup-conflict".to_owned(),
        )],
    )
    .expect("output lease persists");
    database
        .connect()
        .execute(
            "UPDATE store_leases SET state = 'released', released_at = transaction_timestamp() WHERE lease_id = 'startup-conflict-output'",
            &[],
        )
        .expect("output lease releases");
    fs::write(gc_roots.join("startup-conflict-output"), b"conflict")
        .expect("conflicting root creates");

    let output = daemon_command(&socket, 1_000, true, database.url())
        .env("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY", &gc_roots)
        .env("TELCHAR_TEST_STORE_RETENTION", "1")
        .output()
        .expect("daemon command runs");

    assert!(!output.status.success());
    assert!(
        !socket.exists(),
        "daemon became ready after failed reconciliation"
    );
    assert!(fs::metadata(gc_roots.join("startup-conflict-output")).is_ok());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("gateway store retention failed"),
        "{stderr}"
    );
    assert!(!stderr.contains("startup-conflict-output"), "{stderr}");
    assert!(!stderr.contains("startup-conflict-request"), "{stderr}");
    assert!(!stderr.contains("/nix/store/"), "{stderr}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn daemon_rejects_missing_database_before_socket_preparation() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    fs::write(&socket, b"preserve").expect("sentinel writes");

    let output = daemon_command_without_database(&socket, 1_000, true)
        .output()
        .expect("daemon command runs");

    assert!(
        !output.status.success(),
        "daemon accepts missing database URL"
    );
    assert_eq!(fs::read(&socket).expect("sentinel reads"), b"preserve");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("database migration failed")
            .count(),
        1
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn daemon_rejects_empty_and_unreachable_database_before_socket_preparation() {
    for database_url in [
        "",
        "postgresql://telchar@localhost:1/telchar",
        "not-a-postgresql-url",
    ] {
        let root = temporary_root();
        fs::create_dir(&root).expect("fixture root creates");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("fixture root permissions set");
        let socket = root.join("daemon.sock");
        fs::write(&socket, b"preserve").expect("sentinel writes");
        let output = daemon_command(&socket, 1_000, true, database_url)
            .output()
            .expect("daemon command runs");
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            !output.status.success(),
            "daemon accepts invalid database configuration"
        );
        assert_eq!(fs::read(&socket).expect("sentinel reads"), b"preserve");
        assert!(stderr.contains("database migration failed"), "{stderr}");
        if !database_url.is_empty() {
            assert!(
                !stderr.contains(database_url),
                "database URL leaked: {stderr}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn daemon_reports_startup_failure_without_panicking() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    let database = PostgresFixture::start();
    let output = daemon_command(&socket, 1_000, true, database.url())
        .output()
        .expect("daemon command runs");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("daemon runtime directory is not private"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked at"), "{stderr}");
    let _ = fs::remove_dir_all(root);
}
