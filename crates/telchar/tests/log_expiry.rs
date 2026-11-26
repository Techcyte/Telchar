use telchar::backend::BackendKind;

#[test]
fn local_log_age_preserves_active_and_pending_attempts() {
    let database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let connection = telchar::persistence::Database::connect(database.url()).unwrap();
    for (index, state) in [(1, "available"), (2, "pending"), (3, "available")] {
        let drv = format!("/nix/store/77777777777777777777777777777777-expiry{index}.drv");
        telchar::persistence::claim_shared_build(
            database.url(),
            &drv,
            &[index; 32],
            "local",
            BackendKind::Local,
            BackendKind::Local.capabilities(),
            None,
            &["/nix/store/88888888888888888888888888888888-output"],
        )
        .unwrap();
        telchar::persistence::start_shared_build(database.url(), &drv).unwrap();
        let attempt = telchar::persistence::read_shared_build_attempt(database.url(), &drv)
            .unwrap()
            .unwrap();
        let name = format!("{}.log.zst", attempt.attempt_id);
        let path = root.path().join(&name);
        std::fs::write(&path, b"retained").unwrap();
        database.connect().execute("INSERT INTO attempt_log_archives (attempt_id, state, object_key, spool_name, sha256, created_at) VALUES ($1, $2, $3, $4, repeat('a', 64), transaction_timestamp() - interval '8 days')", &[&attempt.attempt_id, &state, &path.to_str().unwrap(), &name]).unwrap();
        if index != 3 {
            database.connect().execute("UPDATE shared_build_attempts SET state = 'failed', completed_at = transaction_timestamp() WHERE attempt_id = $1", &[&attempt.attempt_id]).unwrap();
        }
    }
    let manager =
        telchar::service::archive::AttemptLogs::new(root.path().to_owned(), 1024, 1024, 1, 67584)
            .with_retention_days(7);
    telchar::persistence::expire_local_log_archives(&connection, 7, root.path().to_str().unwrap())
        .unwrap();
    assert!(
        manager.recover(&connection).is_err(),
        "invalid pending spool reports recovery failure"
    );
    let rows = database
        .connect()
        .query(
            "SELECT state, spool_name FROM attempt_log_archives ORDER BY attempt_id",
            &[],
        )
        .unwrap();
    assert_eq!(rows[0].get::<_, String>(0), "expired");
    assert!(rows[0].get::<_, Option<String>>(1).is_none());
    assert_eq!(rows[1].get::<_, String>(0), "pending");
    assert!(root.path().join(rows[1].get::<_, String>(1)).exists());
    assert_eq!(rows[2].get::<_, String>(0), "available");
    assert!(root.path().join(rows[2].get::<_, String>(1)).exists());
}
