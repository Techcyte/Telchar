//! Tests archive acknowledgement before terminal history deletion.

use std::time::Duration;

use telchar::backend::BackendKind;
use telchar::service::archive::FilesystemArchive;
use telchar_test_support::postgres::PostgresFixture;

#[test]
fn terminal_history_survives_delivery_failure_and_archives_before_deletion() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let derivation = "/nix/store/77777777777777777777777777777777-history.drv";
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[4; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/88888888888888888888888888888888-history"],
    )
    .unwrap();
    telchar::persistence::start_shared_build(fixture.url(), derivation).unwrap();
    telchar::persistence::complete_shared_build_failure(
        fixture.url(),
        derivation,
        "build-failure",
        &serde_json::json!({"status": "failed"}),
        Duration::from_secs(3600),
    )
    .unwrap();
    let retention = Duration::from_secs(86400);
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("destination");
    std::fs::create_dir(&destination).unwrap();
    let archive = FilesystemArchive::new(destination.clone()).unwrap();
    let deliver = |key: &str, payload: &[u8]| archive.deliver(key, payload).map(|_| ());
    // A recent result remains available regardless of archive policy.
    assert_eq!(
        telchar::persistence::archive_terminal_history(fixture.url(), retention, 1, deliver)
            .unwrap(),
        0
    );
    fixture.connect().batch_execute(
        "UPDATE shared_builds SET created_at = now() - interval '3 days', started_at = now() - interval '3 days', completed_at = now() - interval '2 days', expires_at = now() - interval '1 day'; UPDATE shared_build_attempts SET created_at = now() - interval '3 days', started_at = now() - interval '3 days', completed_at = now() - interval '2 days'",
    ).unwrap();
    std::fs::write(destination.join("history"), b"unavailable directory").unwrap();
    assert!(
        telchar::persistence::archive_terminal_history(fixture.url(), retention, 1, deliver)
            .is_err()
    );
    assert!(
        telchar::persistence::read_shared_build(fixture.url(), derivation)
            .unwrap()
            .is_some()
    );
    std::fs::remove_file(destination.join("history")).unwrap();
    let mut changed = false;
    assert_eq!(
        telchar::persistence::archive_terminal_history(fixture.url(), retention, 1, |key, payload| {
            let mut connection = fixture.connect();
            connection.batch_execute("SET lock_timeout = '500ms'").unwrap();
            connection.execute(
                "UPDATE shared_builds SET expires_at = now() + interval '1 day' WHERE derivation_path = $1",
                &[&derivation],
            ).expect("archive delivery must not hold build locks");
            changed = true;
            deliver(key, payload)
        }).unwrap(),
        0
    );
    assert!(changed);
    assert!(
        telchar::persistence::read_shared_build(fixture.url(), derivation)
            .unwrap()
            .is_some()
    );
    fixture.connect().execute(
        "UPDATE shared_builds SET expires_at = now() - interval '1 day' WHERE derivation_path = $1",
        &[&derivation],
    ).unwrap();
    fixture.connect().execute(
        "INSERT INTO nomad_execution_obligations (attempt_id) SELECT attempt_id FROM shared_build_attempts WHERE derivation_path = $1",
        &[&derivation],
    ).unwrap();
    assert_eq!(
        telchar::persistence::archive_terminal_history(fixture.url(), retention, 1, |_, _| {
            panic!("unresolved execution must not be archived")
        })
        .unwrap(),
        0
    );
    fixture
        .connect()
        .execute(
            "UPDATE nomad_execution_obligations SET resolved_at = now()",
            &[],
        )
        .unwrap();
    assert_eq!(
        telchar::persistence::archive_terminal_history(
            fixture.url(),
            retention,
            1,
            |key, payload| {
                fixture
                    .connect()
                    .execute(
                        "UPDATE nomad_execution_obligations SET resolved_at = NULL",
                        &[],
                    )
                    .unwrap();
                deliver(key, payload)
            }
        )
        .unwrap(),
        0
    );
    fixture
        .connect()
        .execute(
            "UPDATE nomad_execution_obligations SET resolved_at = now()",
            &[],
        )
        .unwrap();
    fixture.connect().execute(
        "INSERT INTO build_requests (request_id, derivation_path, system) SELECT 'history-' || i, $1, 'x86_64-linux' FROM generate_series(1, 257) i",
        &[&derivation],
    ).unwrap();
    let limit =
        telchar::persistence::archive_terminal_history(fixture.url(), retention, 1, |_, _| {
            panic!("oversized snapshot must not be delivered")
        })
        .unwrap_err();
    assert_eq!(limit.to_string(), "history snapshot record limit exceeded");
    assert!(
        telchar::persistence::read_shared_build(fixture.url(), derivation)
            .unwrap()
            .is_some()
    );
    fixture
        .connect()
        .execute(
            "DELETE FROM build_requests WHERE derivation_path = $1",
            &[&derivation],
        )
        .unwrap();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .unwrap();
    let fenced = telchar::persistence::archive_terminal_history(
        owner.database(),
        retention,
        1,
        |key, payload| {
            deliver(key, payload)?;
            fixture.expire_singleton_ownership("daemon");
            Ok(())
        },
    );
    assert!(
        fenced.is_err(),
        "ownership loss must prevent history deletion"
    );
    assert!(
        telchar::persistence::read_shared_build(fixture.url(), derivation)
            .unwrap()
            .is_some()
    );
    drop(owner);
    let config_path = directory.path().join("service.toml");
    std::fs::write(
        &config_path,
        format!(
            "[history_retention]\nretention_days = 1\nmaximum_builds = 1\ndestination = '{}'\n",
            destination.display(),
        ),
    )
    .unwrap();
    let config =
        telchar::service::config::ServiceConfig::load_with_environment(&config_path, |_| None)
            .unwrap();
    let mut cursor = telchar::persistence::HistoryCursor::default();
    assert_eq!(
        telchar::service::archive::maintain_history(fixture.url(), None, &mut cursor).unwrap(),
        0
    );
    assert_eq!(
        telchar::service::archive::maintain_history(
            fixture.url(),
            config.history_retention(),
            &mut cursor
        )
        .unwrap(),
        1
    );
    assert!(
        telchar::persistence::read_shared_build(fixture.url(), derivation)
            .unwrap()
            .is_none()
    );
    let objects = std::fs::read_dir(destination.join("history")).unwrap();
    let records: Vec<serde_json::Value> = objects
        .flat_map(|entry| {
            std::fs::read_to_string(entry.unwrap().path())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(records.iter().any(|record| {
        record["schema_version"] == 1
            && record["build"]["derivation_path"] == derivation
            && record["attempts"]
                .as_array()
                .is_some_and(|attempts| !attempts.is_empty())
    }));
}
