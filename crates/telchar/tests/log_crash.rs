use std::time::{Duration, Instant};

#[test]
fn process_death_reclaims_interrupted_capture_temporary_file() {
    if let Some(root) = std::env::var_os("TELCHAR_LOG_CRASH_ROOT") {
        let attempt = std::env::var("TELCHAR_LOG_CRASH_ATTEMPT")
            .unwrap()
            .parse()
            .unwrap();
        let (capture, _finished) =
            telchar::service::archive::LogCapture::start(root.into(), attempt, 1024, 1024).unwrap();
        capture.push(b"interrupted builder output");
        loop {
            std::thread::park();
        }
    }
    let database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let drv = "/nix/store/77777777777777777777777777777777-crash.drv";
    let kind = telchar::backend::BackendKind::Local;
    telchar::persistence::claim_shared_build(
        database.url(),
        drv,
        &[7; 32],
        "local",
        kind,
        kind.capabilities(),
        None,
        &["/nix/store/88888888888888888888888888888888-output"],
    )
    .unwrap();
    telchar::persistence::start_shared_build(database.url(), drv).unwrap();
    let attempt = telchar::persistence::read_shared_build_attempt(database.url(), drv)
        .unwrap()
        .unwrap();
    let connection = telchar::persistence::Database::connect(database.url()).unwrap();
    assert!(telchar::persistence::begin_log_archive(&connection, attempt.attempt_id).unwrap());
    let root = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_death_reclaims_interrupted_capture_temporary_file",
            "--nocapture",
        ])
        .env("TELCHAR_LOG_CRASH_ROOT", root.path())
        .env("TELCHAR_LOG_CRASH_ATTEMPT", attempt.attempt_id.to_string())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_dir(root.path()).unwrap().next().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("compressor did not create spool");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    let unrelated = root.path().join("operator-note");
    std::fs::write(&unrelated, b"preserve").unwrap();
    let manager =
        telchar::service::archive::AttemptLogs::new(root.path().to_owned(), 1024, 1024, 1, 67584);
    manager.reconcile_interrupted(&connection).unwrap();
    assert_eq!(
        database
            .connect()
            .query_one("SELECT state, complete FROM attempt_log_archives", &[])
            .unwrap()
            .get::<_, String>(0),
        "failed"
    );
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"preserve");
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        1,
        "interrupted temporary log must be reclaimed"
    );
}
