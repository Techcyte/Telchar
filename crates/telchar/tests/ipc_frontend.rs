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
fn second_daemon_cannot_replace_live_socket() {
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let mut first = daemon_command(&socket, 1_000, false)
        .spawn()
        .expect("first daemon starts");
    wait_for_socket(&socket, &mut first);

    let mut second = daemon_command(&socket, 1_000, true)
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
fn daemon_refuses_to_replace_non_socket_path() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    fs::write(&socket, b"preserve").expect("sentinel writes");
    let output = daemon_command(&socket, 1_000, true)
        .output()
        .expect("daemon command runs");
    assert!(!output.status.success(), "daemon replaced non-socket path");
    assert_eq!(fs::read(&socket).expect("sentinel reads"), b"preserve");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn daemon_reports_startup_failure_without_panicking() {
    let root = temporary_root();
    fs::create_dir(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755))
        .expect("fixture root permissions set");
    let socket = root.join("daemon.sock");
    let output = daemon_command(&socket, 1_000, true)
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
    let mut daemon = daemon_command(&socket, 1_000, true)
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
    let mut daemon = daemon_command_with_uid(
        &socket,
        1_000,
        false,
        rustix::process::getuid().as_raw().wrapping_add(1),
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
}

impl Fixture {
    fn start() -> Self {
        Self::start_with_timeout(1_000)
    }

    fn start_with_timeout(envelope_timeout_ms: u64) -> Self {
        Self::start_mode(envelope_timeout_ms, true, 1)
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
        let mut daemon = daemon_command(&socket, envelope_timeout_ms, once)
            .env("TELCHAR_IPC_MAX_SESSIONS", session_limit.to_string())
            .spawn()
            .expect("daemon starts");
        wait_for_socket(&socket, &mut daemon);
        Self {
            root,
            socket,
            daemon,
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
    write_word(&mut input, CLIENT_WORKER_MAGIC);
    write_word(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_word(&mut input, 0);
    input.flush().expect("client handshake flushes");
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
}

fn daemon_command(socket: &Path, envelope_timeout_ms: u64, once: bool) -> Command {
    daemon_command_with_uid(
        socket,
        envelope_timeout_ms,
        once,
        rustix::process::getuid().as_raw(),
    )
}

fn daemon_command_with_uid(
    socket: &Path,
    envelope_timeout_ms: u64,
    once: bool,
    frontend_uid: u32,
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
