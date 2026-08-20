//! Tests ipc auth contracts and failure boundaries, including accepts socket peer with expected uid.

use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;
use std::time::Duration;

use telchar::service::ipc::{authorize_peer, IpcListener};

#[test]
fn accepts_socket_peer_with_expected_uid() {
    let (_client, server) = UnixStream::pair().expect("socket pair");
    let uid = rustix::process::getuid().as_raw();
    authorize_peer(&server, uid).expect("current user is authorized");
}

#[test]
fn accepts_peer_from_another_pid_namespace() {
    if !Command::new("unshare")
        .args(["--user", "--map-current-user", "--pid", "--fork", "true"])
        .status()
        .is_ok_and(|status| status.success())
    {
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let socket_path = directory.path().join("frontend.sock");
    let current_executable = std::env::current_exe().expect("test executable path");
    let mut server = Command::new("unshare")
        .args(["--user", "--map-current-user", "--pid", "--fork"])
        .arg(current_executable)
        .args([
            "--exact",
            "namespaced_server_authorizes_external_peer",
            "--ignored",
        ])
        .env("TELCHAR_TEST_SOCKET", &socket_path)
        .spawn()
        .expect("namespaced server starts");

    let stream = (0..100)
        .find_map(|_| match UnixStream::connect(&socket_path) {
            Ok(stream) => Some(stream),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::thread::sleep(Duration::from_millis(10));
                None
            }
            Err(error) => panic!("peer connects: {error}"),
        })
        .expect("peer connects to namespaced server");
    drop(stream);
    assert!(server.wait().expect("namespaced server exits").success());
}

#[test]
#[ignore = "helper process for cross-PID-namespace authorization"]
fn namespaced_server_authorizes_external_peer() {
    let socket_path = std::env::var("TELCHAR_TEST_SOCKET").expect("test socket path");
    let listener = UnixListener::bind(socket_path).expect("listener binds");
    let uid = rustix::process::getuid().as_raw();
    IpcListener::from_listener(listener, uid)
        .accept_pending()
        .expect("external peer uid is authorized");
}

#[test]
fn rejects_socket_peer_with_wrong_uid() {
    let (_client, server) = UnixStream::pair().expect("socket pair");
    let uid = rustix::process::getuid().as_raw();
    let wrong_uid = if uid == 0 { 1 } else { 0 };
    let error = authorize_peer(&server, wrong_uid).expect_err("wrong user is denied");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(
        error.to_string(),
        format!("local IPC peer uid {uid} does not match expected uid {wrong_uid}")
    );
}
