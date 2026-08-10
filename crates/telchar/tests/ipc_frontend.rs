use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod support;

use support::postgres::PostgresFixture;

use nix_worker_protocol::{CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC};
use telchar::ipc::{IpcEnvelope, IpcError, IpcListener, RequesterMetadata, IPC_VERSION};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
fn separate_frontend_and_daemon_processes_complete_worker_handshake() {
    let fixture = Fixture::start();
    let mut frontend = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .arg("serve-stdio")
        .env("TELCHAR_IPC_SOCKET", &fixture.socket)
        .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("frontend starts");

    assert_ne!(frontend.id(), fixture.daemon.id());
    let mut input = frontend.stdin.take().expect("frontend stdin");
    write_word(&mut input, CLIENT_WORKER_MAGIC);
    write_word(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_word(&mut input, 0);
    input.flush().expect("client handshake flushes");

    let mut output = frontend.stdout.take().expect("frontend stdout");
    assert_eq!(read_word(&mut output), SERVER_WORKER_MAGIC);
    assert_eq!(read_word(&mut output), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_word(&mut output), 0);

    write_word(&mut input, 0);
    write_word(&mut input, 0);
    input.flush().expect("post-handshake flushes");
    assert_eq!(read_string(&mut output), b"telchar");
    assert_eq!(read_word(&mut output), 0);
    assert_eq!(read_word(&mut output), nix_worker_protocol::STDERR_LAST);

    drop(input);
    assert!(frontend.wait().expect("frontend exits").success());
    fixture.finish_successfully();
}

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
fn daemon_reconciles_expired_output_before_readiness() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    let gc_roots = root.join("gc-roots");
    fs::create_dir(&gc_roots).expect("GC root directory creates");
    fs::set_permissions(&gc_roots, fs::Permissions::from_mode(0o700))
        .expect("GC root directory permissions set");
    let database = PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        database.url(),
        "startup-expiry-request",
        "/nix/store/11111111111111111111111111111111-startup-expiry.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let lease = telchar::persistence::create_request_output_leases(
        database.url(),
        "startup-expiry-request",
        Duration::from_secs(60),
        &[(
            "startup-expiry-output".to_owned(),
            "/nix/store/22222222222222222222222222222222-startup-expiry".to_owned(),
        )],
    )
    .expect("output lease persists")
    .remove(0);
    database
        .connect()
        .execute(
            "UPDATE store_leases SET created_at = transaction_timestamp() - interval '2 minutes', expires_at = transaction_timestamp() - interval '1 minute' WHERE lease_id = 'startup-expiry-output'",
            &[],
        )
        .expect("output deadline expires");
    std::os::unix::fs::symlink(
        "/nix/store/22222222222222222222222222222222-startup-expiry",
        gc_roots.join(&lease.lease_id),
    )
    .expect("output root creates");

    let mut daemon = daemon_command(&socket, 1_000, true, database.url())
        .env("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY", &gc_roots)
        .env("TELCHAR_TEST_STORE_RETENTION", "1")
        .spawn()
        .expect("daemon starts");
    wait_for_socket(&socket, &mut daemon);

    assert!(fs::symlink_metadata(gc_roots.join(&lease.lease_id)).is_err());
    assert_eq!(
        telchar::persistence::read_store_lease(database.url(), &lease.lease_id)
            .expect("lease reads")
            .expect("lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Released
    );
    daemon.kill().expect("daemon stops");
    let _ = daemon.wait();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn daemon_refuses_readiness_when_output_reconciliation_fails() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    let gc_roots = root.join("gc-roots");
    fs::create_dir(&gc_roots).expect("GC root directory creates");
    fs::set_permissions(&gc_roots, fs::Permissions::from_mode(0o700))
        .expect("GC root directory permissions set");
    let database = PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        database.url(),
        "startup-conflict-request",
        "/nix/store/11111111111111111111111111111111-startup-conflict.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_request_output_leases(
        database.url(),
        "startup-conflict-request",
        Duration::from_secs(60),
        &[(
            "startup-conflict-output".to_owned(),
            "/nix/store/22222222222222222222222222222222-startup-conflict".to_owned(),
        )],
    )
    .expect("output lease persists");
    database
        .connect()
        .execute(
            "UPDATE store_leases SET state = 'released', released_at = transaction_timestamp() WHERE lease_id = 'startup-conflict-output'",
            &[],
        )
        .expect("output lease releases");
    fs::write(gc_roots.join("startup-conflict-output"), b"conflict")
        .expect("conflicting root creates");

    let output = daemon_command(&socket, 1_000, true, database.url())
        .env("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY", &gc_roots)
        .env("TELCHAR_TEST_STORE_RETENTION", "1")
        .output()
        .expect("daemon command runs");

    assert!(!output.status.success());
    assert!(
        !socket.exists(),
        "daemon became ready after failed reconciliation"
    );
    assert!(fs::metadata(gc_roots.join("startup-conflict-output")).is_ok());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("gateway store retention failed"),
        "{stderr}"
    );
    assert!(!stderr.contains("startup-conflict-output"), "{stderr}");
    assert!(!stderr.contains("startup-conflict-request"), "{stderr}");
    assert!(!stderr.contains("/nix/store/"), "{stderr}");
    let _ = fs::remove_dir_all(root);
}

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
fn daemon_exits_and_releases_socket_after_ownership_connection_loss() {
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let mut database = PostgresFixture::start();
    let mut daemon = daemon_command(&socket, 1_000, false, database.url())
        .env("TELCHAR_SINGLETON_CHECK_INTERVAL_MS", "10")
        .spawn()
        .expect("daemon starts");
    wait_for_socket(&socket, &mut daemon);

    database.restart();

    let output = wait_with_deadline(&mut daemon, Duration::from_secs(2));
    assert!(
        !output.status.success(),
        "fenced daemon exited successfully"
    );
    assert!(!socket.exists(), "fenced daemon left admission socket open");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("database.singleton_ownership.lost"),
        "{stderr}"
    );
    assert!(!stderr.contains(database.url()), "{stderr}");
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

    database.restart();
    let first_output = wait_with_deadline(&mut first, Duration::from_secs(2));
    assert!(
        !first_output.status.success(),
        "fenced owner exited successfully"
    );
    assert!(!first_socket.exists(), "fenced owner kept admission open");

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

#[test]
fn daemon_restart_recovers_queued_requests_before_readiness_without_duplication() {
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let database = PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        database.url(),
        "process-queued-request",
        "/nix/store/11111111111111111111111111111111-process-queued.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        database.url(),
        "process-queued-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "process-queued-request",
        "/nix/store/11111111111111111111111111111111-process-queued.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        database.url(),
        "process-queued-request",
        &[(
            "process-queued-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::queue_build_request(database.url(), "process-queued-request")
        .expect("request queues");

    for _ in 0..2 {
        let mut daemon = daemon_command(&socket, 1_000, false, database.url())
            .spawn()
            .expect("daemon starts");
        wait_for_socket(&socket, &mut daemon);
        assert_eq!(
            telchar::persistence::recover_queued_build_requests(database.url(), 256)
                .expect("queue reads")
                .iter()
                .map(|request| request.request_id.as_str())
                .collect::<Vec<_>>(),
            vec!["process-queued-request"]
        );
        daemon.kill().expect("daemon stops");
        let _ = daemon.wait();
        let _ = fs::remove_file(&socket);
    }
    let mut client = database.connect();
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM build_requests WHERE queue_state = 'queued'",
                &[]
            )
            .expect("queue count reads")
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM execution_attempts", &[])
            .expect("attempt count reads")
            .get::<_, i64>(0),
        0
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn daemon_restart_recovers_backend_pending_attempt_without_duplication() {
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let database = PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        database.url(),
        "process-pending-request",
        "/nix/store/11111111111111111111111111111111-process-pending.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        database.url(),
        "process-pending-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "process-pending-request",
        "/nix/store/11111111111111111111111111111111-process-pending.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        database.url(),
        "process-pending-request",
        &[(
            "process-pending-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::queue_build_request(database.url(), "process-pending-request")
        .expect("request queues");
    telchar::persistence::dispatch_build_request(
        database.url(),
        "process-pending-request",
        "process-pending-attempt",
        1,
        "process-pending-request:1",
        "local",
        "process-pending-reservation",
        1,
    )
    .expect("request dispatches");
    telchar::persistence::register_local_backend_execution(
        database.url(),
        "local-process-pending",
        "process-pending-request:1",
        &[9_u8; 32],
    )
    .expect("backend execution persists");
    telchar::persistence::record_backend_submission(
        database.url(),
        "process-pending-attempt",
        "local-process-pending",
    )
    .expect("backend submission persists");

    for _ in 0..2 {
        let mut daemon = daemon_command(&socket, 1_000, false, database.url())
            .spawn()
            .expect("daemon starts");
        wait_for_socket(&socket, &mut daemon);
        let recovered = telchar::persistence::recover_backend_pending_attempts(database.url(), 256)
            .expect("pending attempt reads");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].attempt.attempt_id, "process-pending-attempt");
        daemon.kill().expect("daemon stops");
        let _ = daemon.wait();
        let _ = fs::remove_file(&socket);
    }
    let mut client = database.connect();
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM execution_attempts", &[])
            .expect("attempt count reads")
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM local_backend_executions", &[])
            .expect("backend count reads")
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM execution_attempts WHERE attempt_id = 'process-pending-attempt' AND backend_execution_id = 'local-process-pending' AND state = 'backend-pending'",
                &[],
            )
            .expect("pending attempt reads")
            .get::<_, i64>(0),
        1
    );
    let _ = fs::remove_dir_all(root);
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
fn daemon_rejects_missing_database_before_socket_preparation() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    fs::write(&socket, b"preserve").expect("sentinel writes");

    let output = daemon_command_without_database(&socket, 1_000, true)
        .output()
        .expect("daemon command runs");

    assert!(
        !output.status.success(),
        "daemon accepts missing database URL"
    );
    assert_eq!(fs::read(&socket).expect("sentinel reads"), b"preserve");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("database migration failed")
            .count(),
        1
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn daemon_rejects_empty_and_unreachable_database_before_socket_preparation() {
    for database_url in [
        "",
        "postgresql://telchar@localhost:1/telchar",
        "not-a-postgresql-url",
    ] {
        let root = temporary_root();
        fs::create_dir(&root).expect("fixture root creates");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("fixture root permissions set");
        let socket = root.join("daemon.sock");
        fs::write(&socket, b"preserve").expect("sentinel writes");
        let output = daemon_command(&socket, 1_000, true, database_url)
            .output()
            .expect("daemon command runs");
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            !output.status.success(),
            "daemon accepts invalid database configuration"
        );
        assert_eq!(fs::read(&socket).expect("sentinel reads"), b"preserve");
        assert!(stderr.contains("database migration failed"), "{stderr}");
        if !database_url.is_empty() {
            assert!(
                !stderr.contains(database_url),
                "database URL leaked: {stderr}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn daemon_reports_startup_failure_without_panicking() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    let database = PostgresFixture::start();
    let output = daemon_command(&socket, 1_000, true, database.url())
        .output()
        .expect("daemon command runs");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("daemon runtime directory is not private"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked at"), "{stderr}");
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

struct Fixture {
    root: PathBuf,
    socket: PathBuf,
    daemon: Child,
    database: PostgresFixture,
}

impl Fixture {
    fn start() -> Self {
        Self::start_with_timeout(1_000)
    }

    fn start_with_timeout(envelope_timeout_ms: u64) -> Self {
        Self::start_mode(envelope_timeout_ms, true, 1)
    }

    fn start_worker_timeout(worker_timeout_ms: u64) -> Self {
        let mut fixture = Self::start();
        fixture.daemon.kill().expect("default daemon stops");
        let _ = fixture.daemon.wait();
        fs::remove_file(&fixture.socket).expect("default daemon socket removes");
        fixture.daemon = daemon_command(&fixture.socket, 1_000, true, fixture.database.url())
            .env(
                "TELCHAR_WORKER_IDLE_TIMEOUT_MS",
                worker_timeout_ms.to_string(),
            )
            .spawn()
            .expect("timeout daemon starts");
        wait_for_socket(&fixture.socket, &mut fixture.daemon);
        fixture
    }

    fn start_persistent(envelope_timeout_ms: u64) -> Self {
        Self::start_persistent_with_limit(envelope_timeout_ms, 64)
    }

    fn start_persistent_with_limit(envelope_timeout_ms: u64, session_limit: usize) -> Self {
        Self::start_mode(envelope_timeout_ms, false, session_limit)
    }

    fn start_mode(envelope_timeout_ms: u64, once: bool, session_limit: usize) -> Self {
        let root = temporary_root();
        let socket = root.join("daemon.sock");
        let database = PostgresFixture::start();
        let mut daemon = daemon_command(&socket, envelope_timeout_ms, once, database.url())
            .env("TELCHAR_IPC_MAX_SESSIONS", session_limit.to_string())
            .spawn()
            .expect("daemon starts");
        wait_for_socket(&socket, &mut daemon);
        Self {
            root,
            socket,
            daemon,
            database,
        }
    }

    fn spawn_frontend(&self) -> Child {
        Command::new(env!("CARGO_BIN_EXE_telchar"))
            .arg("serve-stdio")
            .env("TELCHAR_IPC_SOCKET", &self.socket)
            .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:fixture")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("frontend starts")
    }

    fn finish_successfully(self) {
        let output = self.daemon.wait_with_output().expect("daemon exits");
        let _ = fs::remove_dir_all(&self.root);
        assert!(output.status.success(), "daemon failed: {output:?}");
        assert!(output.stdout.is_empty(), "daemon wrote to stdout");
    }

    fn finish_with_failure(mut self) {
        let output = wait_with_deadline(&mut self.daemon, Duration::from_secs(2));
        let _ = fs::remove_dir_all(&self.root);
        assert!(!output.status.success(), "daemon accepted invalid envelope");
        assert!(output.stdout.is_empty(), "daemon wrote to stdout");
    }

    fn stop(mut self) {
        self.daemon.kill().expect("daemon stops");
        let _ = self.daemon.wait();
        let _ = fs::remove_dir_all(self.root);
    }
}

fn wait_for_socket(path: &Path, daemon: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !path.exists() {
        assert!(Instant::now() < deadline, "daemon socket was not created");
        assert!(
            daemon.try_wait().expect("daemon status").is_none(),
            "daemon exited before binding"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_with_deadline(child: &mut Child, timeout: Duration) -> std::process::Output {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("child status") {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            child
                .stdout
                .take()
                .expect("child stdout")
                .read_to_end(&mut stdout)
                .expect("child stdout reads");
            child
                .stderr
                .take()
                .expect("child stderr")
                .read_to_end(&mut stderr)
                .expect("child stderr reads");
            return std::process::Output {
                status,
                stdout,
                stderr,
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("child did not exit before deadline");
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn authenticated_envelope(session_id: &str) -> IpcEnvelope {
    IpcEnvelope {
        version: IPC_VERSION,
        requester: RequesterMetadata {
            credential_id: "ssh-pubkey:fixture".into(),
            audit_subject: "fixture".into(),
            quota_subject: "ssh-pubkey:fixture".into(),
        },
        session_id: session_id.into(),
        error: None,
    }
}

fn write_word(output: &mut impl Write, value: u64) {
    output.write_all(&value.to_le_bytes()).expect("word writes");
}

fn read_word(input: &mut impl Read) -> u64 {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes).expect("word reads");
    u64::from_le_bytes(bytes)
}

fn complete_handshake(frontend: &mut Child) {
    let mut input = frontend.stdin.take().expect("frontend stdin");
    let mut output = frontend.stdout.take().expect("frontend stdout");
    complete_worker_handshake(&mut input, &mut output);
    drop(input);
    assert!(frontend.wait().expect("frontend exits").success());
}

fn complete_post_handshake_stream(stream: &mut UnixStream) {
    write_word(stream, 0);
    write_word(stream, 0);
    stream.flush().expect("post-handshake flushes");
    assert_eq!(read_string(stream), b"telchar");
    assert_eq!(read_word(stream), 0);
    assert_eq!(read_word(stream), nix_worker_protocol::STDERR_LAST);
}

fn complete_worker_handshake(input: &mut impl Write, output: &mut impl Read) {
    write_word(input, CLIENT_WORKER_MAGIC);
    write_word(input, LATEST_WORKER_VERSION.to_wire());
    write_word(input, 0);
    input.flush().expect("client handshake flushes");
    assert_eq!(read_word(output), SERVER_WORKER_MAGIC);
    assert_eq!(read_word(output), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_word(output), 0);
    write_word(input, 0);
    write_word(input, 0);
    input.flush().expect("post-handshake flushes");
    assert_eq!(read_string(output), b"telchar");
    assert_eq!(read_word(output), 0);
    assert_eq!(read_word(output), nix_worker_protocol::STDERR_LAST);
}

fn daemon_command(
    socket: &Path,
    envelope_timeout_ms: u64,
    once: bool,
    database_url: &str,
) -> Command {
    daemon_command_with_uid(
        socket,
        envelope_timeout_ms,
        once,
        rustix::process::getuid().as_raw(),
        database_url,
    )
}

fn daemon_command_without_database(socket: &Path, envelope_timeout_ms: u64, once: bool) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_telchar"));
    command.args([
        "daemon",
        "--socket",
        socket.to_str().expect("UTF-8 socket path"),
        "--frontend-uid",
        &rustix::process::getuid().as_raw().to_string(),
    ]);
    if once {
        command.arg("--once");
    }
    command
        .env("TELCHAR_SYSTEM", "x86_64-linux")
        .env("TELCHAR_SUPPORTED_FEATURES", "")
        .env(
            "TELCHAR_IPC_ENVELOPE_TIMEOUT_MS",
            envelope_timeout_ms.to_string(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn daemon_command_with_uid(
    socket: &Path,
    envelope_timeout_ms: u64,
    once: bool,
    frontend_uid: u32,
    database_url: &str,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_telchar"));
    command.args([
        "daemon",
        "--socket",
        socket.to_str().expect("UTF-8 socket path"),
        "--frontend-uid",
        &frontend_uid.to_string(),
    ]);
    if once {
        command.arg("--once");
    }
    command
        .env("TELCHAR_DATABASE_URL", database_url)
        .env("TELCHAR_GATEWAY_STORE_URI", "unix:///run/nix-daemon.sock")
        .env("TELCHAR_SYSTEM", "x86_64-linux")
        .env("TELCHAR_SUPPORTED_FEATURES", "")
        .env(
            "TELCHAR_IPC_ENVELOPE_TIMEOUT_MS",
            envelope_timeout_ms.to_string(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn temporary_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "telchar-ipc-process-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time follows epoch")
            .as_nanos(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn read_string(input: &mut impl Read) -> Vec<u8> {
    let length = read_word(input) as usize;
    let padded = length.div_ceil(8) * 8;
    let mut bytes = vec![0; padded];
    input.read_exact(&mut bytes).expect("string reads");
    bytes.truncate(length);
    bytes
}
