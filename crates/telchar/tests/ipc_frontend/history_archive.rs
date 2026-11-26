//! Tests configured terminal history maintenance in the daemon process.

use super::*;

#[test]
fn daemon_archives_terminal_history_and_defers_unavailable_destination() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let socket = root.path().join("daemon.sock");
    let database = PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let drv = "/nix/store/77777777777777777777777777777777-history.drv";
    telchar::persistence::claim_shared_build(
        database.url(),
        drv,
        &[7; 32],
        "local",
        telchar::backend::BackendKind::Local,
        telchar::backend::BackendKind::Local.capabilities(),
        None,
        &["/nix/store/88888888888888888888888888888888-history"],
    )
    .unwrap();
    telchar::persistence::start_shared_build(database.url(), drv).unwrap();
    telchar::persistence::complete_shared_build_failure(
        database.url(),
        drv,
        "build-failure",
        &serde_json::json!({"status":"failed"}),
        Duration::from_secs(1),
    )
    .unwrap();
    database.connect().batch_execute(
        "UPDATE shared_builds SET created_at = now() - interval '3 days', started_at = now() - interval '3 days', completed_at = now() - interval '2 days', expires_at = now() - interval '1 day'; UPDATE shared_build_attempts SET created_at = now() - interval '3 days', started_at = now() - interval '3 days', completed_at = now() - interval '2 days'",
    ).unwrap();
    let destination = root.path().join("archives");
    let mut command = daemon_command(&socket, 1000, false, database.url());
    let config = socket.with_extension("toml");
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(config).unwrap();
    writeln!(
        file,
        "\n[history_retention]\nretention_days = 1\nmaximum_builds = 1\ndestination = '{}'",
        destination.display()
    )
    .unwrap();
    let mut daemon = command
        .env("TELCHAR_TEST_STORE_RETENTION", "filesystem-only")
        .env("TELCHAR_TEST_MAINTENANCE_INTERVAL_MS", "25")
        .spawn()
        .unwrap();
    let stderr = daemon.stderr.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = thread::spawn(move || {
        use std::io::BufRead;
        let mut diagnostics = String::new();
        for line in std::io::BufReader::new(stderr).lines() {
            let line = line.unwrap();
            if line.contains("history.retention.failed") {
                let _ = sender.send(());
            }
            diagnostics.push_str(&line);
            diagnostics.push('\n');
        }
        diagnostics
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        if let Some(status) = daemon.try_wait().unwrap() {
            let diagnostics = reader.join().unwrap();
            panic!("daemon exited {status}: {diagnostics}");
        }
        thread::sleep(Duration::from_millis(25));
    }
    let deferred = receiver.recv_timeout(Duration::from_secs(10));
    let retained = telchar::persistence::read_shared_build(database.url(), drv)
        .unwrap()
        .is_some();
    fs::create_dir(&destination).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while telchar::persistence::read_shared_build(database.url(), drv)
        .unwrap()
        .is_some()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    let deleted = telchar::persistence::read_shared_build(database.url(), drv)
        .unwrap()
        .is_none();
    let alive = daemon.try_wait().unwrap().is_none();
    if alive {
        daemon.kill().unwrap();
    }
    daemon.wait().unwrap();
    let diagnostics = reader.join().unwrap();
    assert!(
        deferred.is_ok() && retained && deleted && alive,
        "{diagnostics}"
    );
    let entry = fs::read_dir(destination.join("history"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let archive: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(entry.path()).unwrap()).unwrap();
    assert_eq!(archive["build"]["derivation_path"], drv);
}
