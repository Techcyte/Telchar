use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn pinned_nix_completes_worker_handshake_with_serve_stdio() {
    let fixture = Fixture::start("handshake");
    let output = fixture
        .nix_command()
        .args([
            "--extra-experimental-features",
            "nix-command",
            "--store",
            "ssh-ng://telchar-handshake-test",
            "store",
            "info",
        ])
        .output()
        .expect("pinned Nix client runs");

    assert!(output.status.success(), "pinned Nix failed: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Version: telchar"),
        "pinned client did not receive server handshake information: {stderr}"
    );
    fixture.finish();
}

#[test]
fn pinned_nix_reports_framed_error_after_set_options() {
    let fixture = Fixture::start("error");
    let output = fixture
        .nix_command()
        .args([
            "--extra-experimental-features",
            "nix-command",
            "--store",
            "ssh-ng://telchar-error-test",
            "path-info",
            "/nix/store/00000000000000000000000000000000-missing",
        ])
        .output()
        .expect("pinned Nix client runs");

    assert!(
        !output.status.success(),
        "request must receive framed rejection"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported worker operation"),
        "client did not decode worker error: {stderr}"
    );
    assert!(
        !stderr.contains("unexpected end-of-file"),
        "client received EOF instead of worker error: {stderr}"
    );
    fixture.finish();
}

struct Fixture {
    root: PathBuf,
    socket: PathBuf,
    daemon: Child,
}

impl Fixture {
    fn start(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "telchar-stdio-{name}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir(&root).expect("test root creates");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("test root permissions set");
        let ssh = root.join("ssh");
        fs::write(&ssh, "#!/bin/sh\nexec \"$TELCHAR_STDIO_BIN\" serve-stdio\n")
            .expect("SSH shim writes");
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700))
            .expect("SSH shim is executable");
        let socket = root.join("daemon.sock");
        let mut daemon = Command::new(env!("CARGO_BIN_EXE_telchar"))
            .env("TELCHAR_SYSTEM", "x86_64-linux")
            .env("TELCHAR_SUPPORTED_FEATURES", "")
            .args([
                "daemon",
                "--socket",
                socket.to_str().expect("UTF-8 socket path"),
                "--frontend-uid",
                &rustix::process::getuid().as_raw().to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
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

    fn nix_command(&self) -> Command {
        let path = format!(
            "{}:{}",
            self.root.display(),
            std::env::var("PATH").expect("PATH exists")
        );
        let mut command = Command::new(reference_nix());
        command
            .env("PATH", path)
            .env("TELCHAR_STDIO_BIN", env!("CARGO_BIN_EXE_telchar"))
            .env("TELCHAR_IPC_SOCKET", &self.socket)
            .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:fixture");
        command
    }

    fn finish(mut self) {
        self.daemon.kill().expect("daemon stops");
        let output = self.daemon.wait_with_output().expect("daemon exits");
        let _ = fs::remove_dir_all(self.root);
        assert!(
            output.status.code().is_none(),
            "daemon exited before fixture cleanup: {output:?}"
        );
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

fn reference_nix() -> PathBuf {
    std::env::var_os("TELCHAR_NIX_BIN")
        .map(PathBuf::from)
        .expect("TELCHAR_NIX_BIN identifies the flake-pinned Nix client")
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_nanos()
}
