//! Tests IPC ownership.

use super::*;

#[test]
fn second_daemon_is_refused_by_database_ownership_before_socket_binding() {
    let first_root = temporary_root();
    let second_root = temporary_root();
    let first_socket = first_root.join("daemon.sock");
    let second_socket = second_root.join("daemon.sock");
    let database = PostgresFixture::start();
    let mut first = daemon_command(&first_socket, 1_000, false, database.url())
        .spawn()
        .expect("first daemon starts");
    wait_for_socket(&first_socket, &mut first);

    let output = daemon_command(&second_socket, 1_000, true, database.url())
        .output()
        .expect("second daemon runs");

    assert!(!output.status.success(), "second daemon became active");
    assert!(!second_socket.exists(), "contended daemon bound its socket");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("database.singleton_ownership.refused"),
        "{stderr}"
    );
    assert!(!stderr.contains(database.url()), "{stderr}");

    first.kill().expect("first daemon stops");
    let _ = first.wait();
    let _ = fs::remove_dir_all(first_root);
    let _ = fs::remove_dir_all(second_root);
}

#[test]
fn daemon_keeps_lease_after_database_restart() {
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let mut database = PostgresFixture::start();
    let mut daemon = daemon_command(&socket, 1_000, false, database.url())
        .env("TELCHAR_SINGLETON_CHECK_INTERVAL_MS", "10")
        .spawn()
        .expect("daemon starts");
    wait_for_socket(&socket, &mut daemon);

    database.restart();

    std::thread::sleep(Duration::from_millis(1_200));
    assert!(
        daemon.try_wait().expect("daemon status").is_none(),
        "daemon exited after recoverable database restart"
    );
    assert!(
        socket.exists(),
        "daemon removed admission socket after recovery"
    );
    daemon.kill().expect("daemon stops");
    let _ = daemon.wait();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn replacement_daemon_is_refused_until_fenced_owner_exits() {
    let first_root = temporary_root();
    let contended_root = temporary_root();
    let first_socket = first_root.join("daemon.sock");
    let contended_socket = contended_root.join("daemon.sock");
    let database = PostgresFixture::start();
    let mut first = daemon_command(&first_socket, 1_000, false, database.url())
        .spawn()
        .expect("first daemon starts");
    wait_for_socket(&first_socket, &mut first);

    let output = daemon_command(&contended_socket, 1_000, true, database.url())
        .output()
        .expect("contended replacement runs");

    assert!(
        !output.status.success(),
        "replacement overlapped active owner"
    );
    assert!(
        !contended_socket.exists(),
        "contended replacement opened admission"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("database.singleton_ownership.refused")
    );
    assert!(
        first.try_wait().expect("first daemon status").is_none(),
        "active owner stopped during contention"
    );

    first.kill().expect("first daemon stops");
    let _ = first.wait();
    let _ = fs::remove_dir_all(first_root);
    let _ = fs::remove_dir_all(contended_root);
}

#[test]
fn replacement_daemon_starts_only_after_fenced_owner_exits() {
    let first_root = temporary_root();
    let replacement_root = temporary_root();
    let first_socket = first_root.join("daemon.sock");
    let replacement_socket = replacement_root.join("daemon.sock");
    let mut database = PostgresFixture::start();
    let database_url = database.url().to_owned();
    let mut first = daemon_command(&first_socket, 1_000, false, &database_url)
        .env("TELCHAR_SINGLETON_CHECK_INTERVAL_MS", "10")
        .spawn()
        .expect("first daemon starts");
    wait_for_socket(&first_socket, &mut first);

    database.expire_singleton_ownership("daemon");
    let mut replacement = daemon_command(&replacement_socket, 1_000, false, &database_url)
        .spawn()
        .expect("replacement daemon starts");
    wait_for_socket(&replacement_socket, &mut replacement);
    let first_output = wait_with_deadline(&mut first, Duration::from_secs(2));
    assert!(
        !first_output.status.success(),
        "fenced owner exited successfully"
    );
    assert!(!first_socket.exists(), "fenced owner kept admission open");

    assert!(
        replacement
            .try_wait()
            .expect("replacement daemon status")
            .is_none(),
        "replacement daemon did not remain active"
    );

    replacement.kill().expect("replacement daemon stops");
    let _ = replacement.wait();
    let _ = fs::remove_dir_all(first_root);
    let _ = fs::remove_dir_all(replacement_root);
}

#[test]
fn replacement_daemon_starts_after_owner_process_disappears_and_lease_expires() {
    let first_root = temporary_root();
    let replacement_root = temporary_root();
    let first_socket = first_root.join("daemon.sock");
    let replacement_socket = replacement_root.join("daemon.sock");
    let database = PostgresFixture::start();
    let database_url = database.url().to_owned();
    let mut first = daemon_command(&first_socket, 1_000, false, &database_url)
        .spawn()
        .expect("first daemon starts");
    wait_for_socket(&first_socket, &mut first);
    first.kill().expect("owner process disappears");
    let _ = first.wait();

    std::thread::sleep(Duration::from_secs(4));
    let mut replacement = daemon_command(&replacement_socket, 1_000, false, &database_url)
        .spawn()
        .expect("replacement daemon starts");
    wait_for_socket(&replacement_socket, &mut replacement);
    assert!(
        replacement
            .try_wait()
            .expect("replacement daemon status")
            .is_none(),
        "replacement daemon did not remain active"
    );

    replacement.kill().expect("replacement daemon stops");
    let _ = replacement.wait();
    let _ = fs::remove_dir_all(first_root);
    let _ = fs::remove_dir_all(replacement_root);
}

#[test]
fn second_daemon_cannot_replace_live_socket() {
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let database = PostgresFixture::start();
    let mut first = daemon_command(&socket, 1_000, false, database.url())
        .spawn()
        .expect("first daemon starts");
    wait_for_socket(&socket, &mut first);

    let mut second = daemon_command(&socket, 1_000, true, database.url())
        .spawn()
        .expect("second daemon runs");
    let output = wait_with_deadline(&mut second, Duration::from_millis(500));
    assert!(
        !output.status.success(),
        "second daemon replaced live socket"
    );

    let mut frontend = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .arg("serve-stdio")
        .env("TELCHAR_IPC_SOCKET", &socket)
        .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("frontend starts through original daemon");
    complete_handshake(&mut frontend);
    assert!(
        first.try_wait().expect("first daemon status").is_none(),
        "original daemon stopped serving"
    );

    first.kill().expect("first daemon stops");
    let _ = first.wait();
    let _ = fs::remove_dir_all(root);
}
