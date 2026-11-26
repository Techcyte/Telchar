use std::time::Duration;
use telchar::backend::BackendKind;

#[test]
fn discard_preserves_oversized_history_and_advances_to_eligible_build() {
    let database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let paths = [
        "/nix/store/11111111111111111111111111111111-large.drv",
        "/nix/store/22222222222222222222222222222222-small.drv",
    ];
    for (index, path) in paths.iter().enumerate() {
        telchar::persistence::claim_shared_build(
            database.url(),
            path,
            &[index as u8; 32],
            "local",
            BackendKind::Local,
            BackendKind::Local.capabilities(),
            None,
            &["/nix/store/88888888888888888888888888888888-output"],
        )
        .unwrap();
        telchar::persistence::start_shared_build(database.url(), path).unwrap();
        telchar::persistence::complete_shared_build_failure(
            database.url(),
            path,
            "build-failure",
            &serde_json::json!({"status":"failed"}),
            Duration::from_secs(1),
        )
        .unwrap();
    }
    database.connect().batch_execute("UPDATE shared_builds SET created_at = now() - interval '4 days', started_at = now() - interval '4 days', completed_at = now() - interval '3 days', expires_at = now() - interval '2 days'; UPDATE shared_build_attempts SET created_at = now() - interval '4 days', started_at = now() - interval '4 days', completed_at = now() - interval '3 days'").unwrap();
    database.connect().execute("INSERT INTO build_requests (request_id, derivation_path, system) SELECT 'discard-' || i, $1, 'x86_64-linux' FROM generate_series(1, 257) i", &[&paths[0]]).unwrap();
    let root = tempfile::tempdir().unwrap();
    let config_file = root.path().join("service.toml");
    std::fs::write(
        &config_file,
        "[history_retention]\nretention_days = 1\nmaximum_builds = 1\ndiscard = true\n",
    )
    .unwrap();
    let config =
        telchar::service::config::ServiceConfig::load_with_environment(&config_file, |_| None)
            .unwrap();
    let mut cursor = telchar::persistence::HistoryCursor::default();
    let error = telchar::service::archive::maintain_history(
        database.url(),
        config.history_retention(),
        &mut cursor,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "history snapshot record limit exceeded");
    assert_eq!(
        telchar::service::archive::maintain_history(
            database.url(),
            config.history_retention(),
            &mut cursor
        )
        .unwrap(),
        1
    );
    assert!(
        telchar::persistence::read_shared_build(database.url(), paths[0])
            .unwrap()
            .is_some()
    );
    assert!(
        telchar::persistence::read_shared_build(database.url(), paths[1])
            .unwrap()
            .is_none()
    );
}
