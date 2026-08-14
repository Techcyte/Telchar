//! Tests ipc frontend contracts and failure boundaries, including separate frontend and daemon processes complete worker handshake.

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
use telchar::service::ipc::{IpcEnvelope, IpcError, IpcListener, RequesterMetadata, IPC_VERSION};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[path = "ipc_frontend/handshake.rs"]
mod handshake;
#[path = "ipc_frontend/ownership.rs"]
mod ownership;
#[path = "ipc_frontend/readiness.rs"]
mod readiness;
#[path = "ipc_frontend/sessions.rs"]
mod sessions;
#[path = "ipc_frontend/socket.rs"]
mod socket;

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
        if let Some(status) = daemon.try_wait().expect("daemon status") {
            let mut stderr = String::new();
            std::io::Read::read_to_string(
                daemon.stderr.as_mut().expect("daemon stderr"),
                &mut stderr,
            )
            .expect("daemon stderr reads");
            panic!("daemon exited before binding: {status}: {stderr}");
        }
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
    let socket_parent = socket.parent().expect("socket has parent");
    if !socket_parent.exists() {
        fs::create_dir_all(socket_parent).expect("socket parent creates");
        fs::set_permissions(socket_parent, fs::Permissions::from_mode(0o700))
            .expect("socket parent permissions set");
    }
    let config = socket.with_extension("toml");
    fs::write(
        &config,
        "[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n",
    )
    .expect("daemon configuration writes");
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
        .env("TELCHAR_CONFIG", config)
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
    let socket_parent = socket.parent().expect("socket has parent");
    if !socket_parent.exists() {
        fs::create_dir_all(socket_parent).expect("socket parent creates");
        fs::set_permissions(socket_parent, fs::Permissions::from_mode(0o700))
            .expect("socket parent permissions set");
    }
    let config = socket.with_extension("toml");
    fs::write(
        &config,
        "[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n",
    )
    .expect("daemon configuration writes");
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
        .env("TELCHAR_CONFIG", config)
        .env("TELCHAR_DATABASE_URL", database_url)
        .env("TELCHAR_GATEWAY_STORE_URI", "unix:///run/nix-daemon.sock")
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
