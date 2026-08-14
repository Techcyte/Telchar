//! Tests IPC socket.

use super::*;

#[test]
fn daemon_secures_socket_path_and_cleans_up_after_once() {
    let fixture = Fixture::start();
    assert_eq!(
        fs::metadata(&fixture.root)
            .expect("runtime directory metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let socket_metadata = fs::symlink_metadata(&fixture.socket).expect("socket metadata");
    assert!(socket_metadata.file_type().is_socket());
    assert_eq!(socket_metadata.permissions().mode() & 0o777, 0o600);

    let mut frontend = fixture.spawn_frontend();
    complete_handshake(&mut frontend);
    let socket = fixture.socket.clone();
    fixture.finish_successfully();
    assert!(
        !socket.exists(),
        "daemon socket remains after normal shutdown"
    );
}

#[test]
fn daemon_refuses_to_replace_non_socket_path() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    fs::write(&socket, b"preserve").expect("sentinel writes");
    let database = PostgresFixture::start();
    let output = daemon_command(&socket, 1_000, true, database.url())
        .output()
        .expect("daemon command runs");
    assert!(!output.status.success(), "daemon replaced non-socket path");
    assert_eq!(fs::read(&socket).expect("sentinel reads"), b"preserve");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn daemon_refuses_insecure_existing_runtime_directory_without_changing_it() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    let database = PostgresFixture::start();
    let mut daemon = daemon_command(&socket, 1_000, true, database.url())
        .spawn()
        .expect("daemon command runs");
    let output = wait_with_deadline(&mut daemon, Duration::from_millis(500));
    assert!(
        !output.status.success(),
        "daemon accepted insecure runtime directory"
    );
    assert_eq!(
        fs::metadata(&root)
            .expect("runtime directory metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "daemon changed existing runtime directory permissions"
    );
    assert!(!socket.exists());
    let _ = fs::remove_dir_all(root);
}
