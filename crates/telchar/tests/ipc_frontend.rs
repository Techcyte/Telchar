use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;

use telchar::ipc::{IPC_VERSION, IpcEnvelope, IpcListener, RequesterMetadata, StreamAttachment};

fn envelope() -> IpcEnvelope {
    IpcEnvelope {
        version: IPC_VERSION,
        requester: RequesterMetadata {
            credential_id: "ssh-pubkey:fixture".into(),
            audit_subject: "fixture".into(),
            quota_subject: "fixture".into(),
        },
        session_id: "session-frontend".into(),
        attachment: StreamAttachment { id: 7 },
        error: None,
    }
}

#[test]
fn frontend_connects_and_forwards_protocol_stream_after_envelope() {
    let path = std::env::temp_dir().join(format!("telchar-ipc-{}", std::process::id()));
    let listener = UnixListener::bind(&path).expect("daemon listener binds");
    let expected_uid = rustix::process::getuid().as_raw();
    let daemon = thread::spawn(move || {
        let listener = IpcListener::from_listener(listener, expected_uid);
        let mut connection = listener.accept().expect("frontend accepted");
        assert_eq!(connection.envelope(), &envelope());
        let mut byte = [0; 4];
        connection
            .stream_mut()
            .read_exact(&mut byte)
            .expect("protocol reads");
        assert_eq!(&byte, b"PING");
        connection
            .stream_mut()
            .write_all(b"PONG")
            .expect("protocol writes");
        connection.peer_pid().expect("peer pid recorded")
    });

    let mut frontend = UnixStream::connect(&path).expect("frontend connects");
    IpcListener::send_envelope(&mut frontend, &envelope()).expect("envelope sends");
    frontend.write_all(b"PING").expect("protocol writes");
    let mut response = [0; 4];
    frontend
        .read_exact(&mut response)
        .expect("protocol response reads");
    assert_eq!(&response, b"PONG");
    let peer_pid = daemon.join().expect("daemon completes");
    assert_eq!(peer_pid, std::process::id());
    let _ = std::fs::remove_file(path);
}
