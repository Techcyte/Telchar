//! Tests bounded asynchronous attempt-log capture.

use std::time::Duration;

use telchar::service::archive::LogCapture;

#[test]
fn compressed_log_capture_preserves_attempt_and_marks_byte_limit() {
    let directory = tempfile::tempdir().unwrap();
    let (capture, finished) = LogCapture::start(directory.path().to_owned(), 42, 5, 1024).unwrap();
    capture.push(b"hello world");
    capture.finish();
    let receipt = finished
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(receipt.attempt_id, 42);
    assert!(!receipt.complete);
    assert_eq!(receipt.bytes, 5);
    let decoded = zstd::stream::decode_all(std::fs::File::open(&receipt.path).unwrap()).unwrap();
    assert_eq!(decoded, b"hello");
}

#[test]
fn attempt_capture_persists_reference_without_subscribers() {
    use telchar::backend::BackendKind;
    let database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let drv = "/nix/store/77777777777777777777777777777777-logs.drv";
    telchar::persistence::claim_shared_build(
        database.url(),
        drv,
        &[7; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/88888888888888888888888888888888-output"],
    )
    .unwrap();
    telchar::persistence::start_shared_build(database.url(), drv).unwrap();
    let attempt = telchar::persistence::read_shared_build_attempt(database.url(), drv)
        .unwrap()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let connection = telchar::persistence::Database::connect(database.url()).unwrap();
    let existing = directory.path().join("retained.log.zst");
    std::fs::write(&existing, vec![0; 65536]).unwrap();
    let full = telchar::service::archive::AttemptLogs::new(
        directory.path().to_owned(),
        1024,
        1024,
        1,
        67584,
    );
    assert!(full.begin(&connection, attempt.attempt_id).is_none());
    assert_eq!(
        database
            .connect()
            .query_one("SELECT count(*) FROM attempt_log_archives", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(
        telchar::persistence::stage_log_archive(
            &connection,
            attempt.attempt_id,
            true,
            0,
            "../foreign.log.zst",
            &"a".repeat(64)
        )
        .is_err()
    );
    std::fs::remove_file(existing).unwrap();
    let archive_directory = tempfile::tempdir().unwrap();
    let destination = archive_directory.path().join("destination");
    let registry = telchar::shared_build::SharedBuildRegistry::new().with_attempt_logs(
        telchar::service::archive::AttemptLogs::new(
            directory.path().to_owned(),
            1024,
            1024,
            1,
            67584,
        )
        .with_destination(Some(destination.clone()), None),
    );
    let capture = registry
        .begin_attempt_logs(&connection, attempt.attempt_id)
        .unwrap();
    capture.push(b"retained without readers");
    drop(capture);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let row = database
            .connect()
            .query_one(
                "SELECT state, object_key FROM attempt_log_archives WHERE attempt_id = $1",
                &[&attempt.attempt_id],
            )
            .unwrap();
        if row.get::<_, String>(0) == "pending" {
            std::fs::create_dir(&destination).unwrap();
            let recovery = telchar::service::archive::AttemptLogs::new(
                directory.path().to_owned(),
                1024,
                1024,
                1,
                67584,
            )
            .with_destination(Some(destination.clone()), None);
            recovery.recover(&connection).unwrap();
            continue;
        }
        if row.get::<_, String>(0) == "available" {
            let path: String = row.get(1);
            assert_eq!(
                zstd::stream::decode_all(std::fs::File::open(&path).unwrap()).unwrap(),
                b"retained without readers"
            );
            let spool = directory
                .path()
                .join(format!("{}.log.zst", attempt.attempt_id));
            while spool.exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                !spool.exists(),
                "acknowledged destination must release spool"
            );
            assert!(
                telchar::persistence::finish_log_archive(
                    &connection,
                    attempt.attempt_id,
                    true,
                    0,
                    "unacknowledged",
                    "checksum"
                )
                .is_err()
            );
            std::fs::copy(&path, &spool).unwrap();
            database
                .connect()
                .execute(
                    "UPDATE attempt_log_archives SET spool_name = $1 WHERE attempt_id = $2",
                    &[
                        &spool.file_name().unwrap().to_str().unwrap(),
                        &attempt.attempt_id,
                    ],
                )
                .unwrap();
            let restarted = telchar::service::archive::AttemptLogs::new(
                directory.path().to_owned(),
                1024,
                1024,
                1,
                67584,
            )
            .with_destination(Some(destination.clone()), None);
            let retained_directory = directory.path().with_extension("retained");
            std::fs::rename(directory.path(), &retained_directory).unwrap();
            let foreign = tempfile::tempdir().unwrap();
            let foreign_file = foreign.path().join(spool.file_name().unwrap());
            std::fs::write(&foreign_file, b"unrelated data").unwrap();
            std::os::unix::fs::symlink(foreign.path(), directory.path()).unwrap();
            let _ = restarted.recover(&connection);
            assert_eq!(std::fs::read(&foreign_file).unwrap(), b"unrelated data");
            std::fs::remove_file(directory.path()).unwrap();
            std::fs::rename(&retained_directory, directory.path()).unwrap();
            restarted.recover(&connection).unwrap();
            restarted.recover(&connection).unwrap();
            assert!(!spool.exists(), "restart must reclaim acknowledged spool");
            let successor_drv = "/nix/store/99999999999999999999999999999999-logs.drv";
            telchar::persistence::claim_shared_build(
                database.url(),
                successor_drv,
                &[9; 32],
                "local",
                BackendKind::Local,
                BackendKind::Local.capabilities(),
                None,
                &["/nix/store/88888888888888888888888888888888-output"],
            )
            .unwrap();
            telchar::persistence::start_shared_build(database.url(), successor_drv).unwrap();
            let successor =
                telchar::persistence::read_shared_build_attempt(database.url(), successor_drv)
                    .unwrap()
                    .unwrap();
            let capture = restarted
                .begin(&connection, successor.attempt_id)
                .expect("reclaimed bytes must admit another full reservation");
            capture.push(b"successor attempt");
            capture.finish();
            loop {
                let state: String = database
                    .connect()
                    .query_one(
                        "SELECT state FROM attempt_log_archives WHERE attempt_id = $1",
                        &[&successor.attempt_id],
                    )
                    .unwrap()
                    .get(0);
                if state == "available" {
                    break;
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
            database
                .connect()
                .execute(
                    "DELETE FROM attempt_log_archives WHERE attempt_id = $1",
                    &[&successor.attempt_id],
                )
                .unwrap();
            std::fs::copy(path, spool).unwrap();
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    database.connect().execute("UPDATE attempt_log_archives SET state = 'capturing', object_key = NULL, complete = true", &[]).unwrap();
    let recovery = telchar::service::archive::AttemptLogs::new(
        directory.path().to_owned(),
        1024,
        1024,
        1,
        67584,
    )
    .with_destination(Some(destination), None);
    recovery.reconcile_interrupted(&connection).unwrap();
    let row = database
        .connect()
        .query_one("SELECT state, complete FROM attempt_log_archives", &[])
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "pending");
    assert!(!row.get::<_, bool>(1));
    recovery.recover(&connection).unwrap();
    database
        .connect()
        .execute(
            "UPDATE attempt_log_archives SET state = 'capturing', object_key = NULL",
            &[],
        )
        .unwrap();
    assert!(
        !directory
            .path()
            .join(format!("{}.log.zst", attempt.attempt_id))
            .exists()
    );
    recovery.reconcile_interrupted(&connection).unwrap();
    assert_eq!(
        database
            .connect()
            .query_one("SELECT state FROM attempt_log_archives", &[])
            .unwrap()
            .get::<_, String>(0),
        "failed"
    );
}

#[test]
fn interrupted_capture_cannot_claim_complete_log() {
    let directory = tempfile::tempdir().unwrap();
    let (capture, finished) =
        LogCapture::start(directory.path().to_owned(), 44, 1024, 1024).unwrap();
    capture.push(b"partial");
    drop(capture);
    assert!(
        !finished
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .complete
    );
}

#[test]
fn log_sink_failure_does_not_fail_producer() {
    let directory = tempfile::tempdir().unwrap();
    let foreign = tempfile::tempdir().unwrap();
    let redirected = directory.path().join("redirected");
    std::os::unix::fs::symlink(foreign.path(), &redirected).unwrap();
    let (capture, finished) = LogCapture::start(redirected, 44, 1024, 1024).unwrap();
    capture.push(b"sensitive output");
    capture.finish();
    assert!(
        finished
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
    assert_eq!(std::fs::read_dir(foreign.path()).unwrap().count(), 0);
    let blocked = directory.path().join("blocked");
    std::fs::write(&blocked, b"not a directory").unwrap();
    let (capture, finished) = LogCapture::start(blocked, 43, 1024, 1024).unwrap();
    capture.push(b"build continues");
    drop(capture);
    assert!(
        finished
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
}
