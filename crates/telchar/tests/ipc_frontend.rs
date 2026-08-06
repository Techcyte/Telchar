use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC,
};

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
    assert_eq!(read_word(&mut output), 0);
    assert_eq!(read_string(&mut output), b"telchar");
    assert_eq!(read_word(&mut output), 0);

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
    stream.write_all(&8_u32.to_le_bytes()).expect("length writes");
    stream.write_all(&[b'T']).expect("partial envelope writes");
    stalled.finish_with_failure();
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
        let root = std::env::temp_dir().join(format!(
            "telchar-ipc-process-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time follows epoch")
                .as_nanos()
        ));
        fs::create_dir(&root).expect("fixture root creates");
        let socket = root.join("daemon.sock");
        let mut daemon = Command::new(env!("CARGO_BIN_EXE_telchar"))
            .args([
                "daemon",
                "--socket",
                socket.to_str().expect("UTF-8 socket path"),
                "--frontend-uid",
                &rustix::process::getuid().as_raw().to_string(),
                "--once",
            ])
            .env(
                "TELCHAR_IPC_ENVELOPE_TIMEOUT_MS",
                envelope_timeout_ms.to_string(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("daemon starts");
        wait_for_socket(&socket, &mut daemon);
        Self {
            root,
            socket,
            daemon,
        }
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
}

fn wait_for_socket(path: &Path, daemon: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !path.exists() {
        assert!(Instant::now() < deadline, "daemon socket was not created");
        assert!(daemon.try_wait().expect("daemon status").is_none(), "daemon exited before binding");
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
    output
        .write_all(&value.to_le_bytes())
        .expect("word writes");
}

fn read_word(input: &mut impl Read) -> u64 {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes).expect("word reads");
    u64::from_le_bytes(bytes)
}

fn read_string(input: &mut impl Read) -> Vec<u8> {
    let length = read_word(input) as usize;
    let padded = length.div_ceil(8) * 8;
    let mut bytes = vec![0; padded];
    input.read_exact(&mut bytes).expect("string reads");
    bytes.truncate(length);
    bytes
}
