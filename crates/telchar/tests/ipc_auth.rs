//! Tests ipc auth contracts and failure boundaries, including accepts socket peer with expected uid.

use std::os::unix::net::UnixStream;

use telchar::service::ipc::authorize_peer;

#[test]
fn accepts_socket_peer_with_expected_uid() {
    let (_client, server) = UnixStream::pair().expect("socket pair");
    let uid = rustix::process::getuid().as_raw();
    authorize_peer(&server, uid).expect("current user is authorized");
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
