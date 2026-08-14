//! Tests IPC sessions.

use super::*;

#[test]
fn daemon_persists_authenticated_session_before_worker_handshake_and_closes_it_after_completion() {
    let fixture = Fixture::start();
    let session_id = "durable-session";
    let mut stream = UnixStream::connect(&fixture.socket).expect("raw frontend connects");
    IpcListener::send_envelope(
        &mut stream,
        &IpcEnvelope {
            version: IPC_VERSION,
            requester: RequesterMetadata {
                credential_id: "ssh-pubkey:fixture".into(),
                audit_subject: "fixture".into(),
                quota_subject: "ssh-pubkey:fixture".into(),
            },
            session_id: session_id.into(),
            error: None,
        },
    )
    .expect("envelope sends");
    write_word(&mut stream, CLIENT_WORKER_MAGIC);
    write_word(&mut stream, LATEST_WORKER_VERSION.to_wire());
    write_word(&mut stream, 0);
    stream.flush().expect("worker handshake flushes");
    assert_eq!(read_word(&mut stream), SERVER_WORKER_MAGIC);
    assert_eq!(read_word(&mut stream), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_word(&mut stream), 0);
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.database.url(), session_id)
            .expect("session reads")
            .expect("session opens before worker handshake")
            .state,
        telchar::persistence::ProtocolSessionState::Open
    );

    complete_post_handshake_stream(&mut stream);
    drop(stream);
    let output = fixture.daemon.wait_with_output().expect("daemon exits");
    assert!(output.status.success(), "daemon failed: {output:?}");
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.database.url(), session_id)
            .expect("session reads")
            .expect("session remains")
            .state,
        telchar::persistence::ProtocolSessionState::Closed
    );
    let _ = fs::remove_dir_all(fixture.root);
}

#[test]
fn daemon_closes_authenticated_session_after_worker_protocol_failure() {
    let fixture = Fixture::start();
    let session_id = "invalid-protocol-session";
    let mut stream = UnixStream::connect(&fixture.socket).expect("raw frontend connects");
    IpcListener::send_envelope(
        &mut stream,
        &IpcEnvelope {
            version: IPC_VERSION,
            requester: RequesterMetadata {
                credential_id: "ssh-pubkey:fixture".into(),
                audit_subject: "fixture".into(),
                quota_subject: "ssh-pubkey:fixture".into(),
            },
            session_id: session_id.into(),
            error: None,
        },
    )
    .expect("envelope sends");
    write_word(&mut stream, 0);
    stream.flush().expect("invalid protocol flushes");
    drop(stream);

    let output = fixture.daemon.wait_with_output().expect("daemon exits");
    assert!(
        !output.status.success(),
        "daemon accepted invalid worker protocol"
    );
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.database.url(), session_id)
            .expect("session reads")
            .expect("session remains")
            .state,
        telchar::persistence::ProtocolSessionState::Closed
    );
    let _ = fs::remove_dir_all(fixture.root);
}

#[test]
fn daemon_closes_authenticated_session_after_worker_timeout() {
    let fixture = Fixture::start_worker_timeout(20);
    let session_id = "timed-out-session";
    let mut stream = UnixStream::connect(&fixture.socket).expect("raw frontend connects");
    IpcListener::send_envelope(&mut stream, &authenticated_envelope(session_id))
        .expect("envelope sends");
    write_word(&mut stream, CLIENT_WORKER_MAGIC);
    write_word(&mut stream, LATEST_WORKER_VERSION.to_wire());
    write_word(&mut stream, 0);
    stream.flush().expect("worker handshake flushes");
    assert_eq!(read_word(&mut stream), SERVER_WORKER_MAGIC);
    assert_eq!(read_word(&mut stream), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_word(&mut stream), 0);
    write_word(&mut stream, 44);
    write_word(&mut stream, 0);
    write_word(&mut stream, 1);
    write_word(&mut stream, 8);
    write_word(&mut stream, 1);
    stream.flush().expect("incomplete request flushes");

    let output = fixture.daemon.wait_with_output().expect("daemon exits");
    assert!(
        output.status.success(),
        "daemon failed after worker timeout: {output:?}"
    );
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.database.url(), session_id)
            .expect("session reads")
            .expect("session remains")
            .state,
        telchar::persistence::ProtocolSessionState::Closed
    );
    let _ = fs::remove_dir_all(fixture.root);
}

#[test]
fn daemon_closes_authenticated_session_after_requester_disconnect() {
    let fixture = Fixture::start();
    let session_id = "disconnected-session";
    let mut stream = UnixStream::connect(&fixture.socket).expect("raw frontend connects");
    IpcListener::send_envelope(
        &mut stream,
        &IpcEnvelope {
            version: IPC_VERSION,
            requester: RequesterMetadata {
                credential_id: "ssh-pubkey:fixture".into(),
                audit_subject: "fixture".into(),
                quota_subject: "ssh-pubkey:fixture".into(),
            },
            session_id: session_id.into(),
            error: None,
        },
    )
    .expect("envelope sends");
    drop(stream);

    let output = fixture.daemon.wait_with_output().expect("daemon exits");
    assert!(
        !output.status.success(),
        "daemon accepted requester disconnect"
    );
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.database.url(), session_id)
            .expect("session reads")
            .expect("session remains")
            .state,
        telchar::persistence::ProtocolSessionState::Closed
    );
    let _ = fs::remove_dir_all(fixture.root);
}

#[test]
fn daemon_rejects_duplicate_authenticated_session_before_worker_protocol() {
    let fixture = Fixture::start_persistent(1_000);
    let session_id = "duplicate-session";
    let mut first = UnixStream::connect(&fixture.socket).expect("first frontend connects");
    IpcListener::send_envelope(&mut first, &authenticated_envelope(session_id))
        .expect("first envelope sends");
    write_word(&mut first, CLIENT_WORKER_MAGIC);
    write_word(&mut first, LATEST_WORKER_VERSION.to_wire());
    write_word(&mut first, 0);
    first.flush().expect("first handshake flushes");
    assert_eq!(read_word(&mut first), SERVER_WORKER_MAGIC);
    assert_eq!(read_word(&mut first), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_word(&mut first), 0);
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.database.url(), session_id)
            .expect("first session reads")
            .expect("first session opens")
            .state,
        telchar::persistence::ProtocolSessionState::Open
    );

    let mut second = UnixStream::connect(&fixture.socket).expect("second frontend connects");
    IpcListener::send_envelope(&mut second, &authenticated_envelope(session_id))
        .expect("second envelope sends");
    second
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("response timeout sets");
    let mut byte = [0; 1];
    assert_eq!(second.read(&mut byte).unwrap_or(0), 0);
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.database.url(), session_id)
            .expect("session reads")
            .expect("session remains")
            .state,
        telchar::persistence::ProtocolSessionState::Open
    );
    drop(first);
    fixture.stop();
}

#[test]
fn daemon_rejects_oversized_and_stalled_envelopes_before_worker_protocol() {
    let oversized = Fixture::start();
    let mut stream = UnixStream::connect(&oversized.socket).expect("raw frontend connects");
    stream
        .write_all(&u32::MAX.to_le_bytes())
        .expect("oversized length writes");
    drop(stream);
    oversized.finish_with_failure();

    let stalled = Fixture::start_with_timeout(40);
    let mut stream = UnixStream::connect(&stalled.socket).expect("raw frontend connects");
    stream
        .write_all(&8_u32.to_le_bytes())
        .expect("length writes");
    stream.write_all(b"T").expect("partial envelope writes");
    stalled.finish_with_failure();
}

#[test]
fn daemon_rejects_frontend_error_envelope_before_worker_protocol() {
    let fixture = Fixture::start();
    let mut stream = UnixStream::connect(&fixture.socket).expect("frontend connects");
    IpcListener::send_envelope(
        &mut stream,
        &IpcEnvelope {
            version: IPC_VERSION,
            requester: RequesterMetadata {
                credential_id: "ssh-pubkey:fixture".into(),
                audit_subject: "fixture".into(),
                quota_subject: "ssh-pubkey:fixture".into(),
            },
            session_id: "failed-session".into(),
            error: Some(IpcError {
                code: "identity-unavailable".into(),
                message: "frontend could not attach requester".into(),
            }),
        },
    )
    .expect("error envelope sends");
    write_word(&mut stream, CLIENT_WORKER_MAGIC);
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("response timeout sets");
    let mut byte = [0; 1];
    assert_eq!(stream.read(&mut byte).unwrap_or(0), 0);
    fixture.finish_with_failure();
}

#[test]
fn stalled_envelope_does_not_block_another_frontend() {
    let fixture = Fixture::start_persistent(1_000);
    let mut stalled = UnixStream::connect(&fixture.socket).expect("stalled frontend connects");
    stalled
        .write_all(&8_u32.to_le_bytes())
        .expect("stalled length writes");
    stalled.write_all(b"T").expect("partial envelope writes");

    let started = Instant::now();
    let mut frontend = fixture.spawn_frontend();
    complete_handshake(&mut frontend);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "valid frontend waited for stalled envelope timeout"
    );
    fixture.stop();
}

#[test]
fn daemon_rejects_connections_beyond_bounded_session_capacity() {
    let fixture = Fixture::start_persistent_with_limit(1_000, 1);
    let _stalled = UnixStream::connect(&fixture.socket).expect("first frontend connects");
    thread::sleep(Duration::from_millis(20));

    let mut excess = UnixStream::connect(&fixture.socket).expect("excess frontend connects");
    excess
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("excess timeout sets");
    excess
        .write_all(&8_u32.to_le_bytes())
        .expect("excess length writes");
    excess
        .write_all(b"T")
        .expect("excess partial envelope writes");
    let mut byte = [0; 1];
    let rejected = match excess.read(&mut byte) {
        Ok(0) => true,
        Err(error) => !matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        Ok(_) => false,
    };
    assert!(rejected, "excess connection remained admitted");
    fixture.stop();
}

#[test]
fn rejected_peer_does_not_terminate_persistent_daemon() {
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let database = PostgresFixture::start();
    let mut daemon = daemon_command_with_uid(
        &socket,
        1_000,
        false,
        rustix::process::getuid().as_raw().wrapping_add(1),
        database.url(),
    )
    .spawn()
    .expect("daemon starts");
    wait_for_socket(&socket, &mut daemon);
    let _rejected = UnixStream::connect(&socket).expect("rejected peer connects");
    thread::sleep(Duration::from_millis(50));
    assert!(
        daemon.try_wait().expect("daemon status").is_none(),
        "peer rejection terminated persistent daemon"
    );
    daemon.kill().expect("daemon stops");
    let _ = daemon.wait();
    let _ = fs::remove_dir_all(root);
}
