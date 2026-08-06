use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn arbitrary_ssh_command_is_replaced_by_forced_command() {
    let fixture = Fixture::start();
    let output = fixture
        .ssh_command()
        .arg("arbitrary-command")
        .output()
        .expect("SSH command runs");

    assert!(
        !output
            .stdout
            .windows(b"arbitrary-command".len())
            .any(|window| window == b"arbitrary-command")
    );
    assert!(
        fs::read_to_string(fixture.root.join("forced-command-output"))
            .expect("forced command evidence reads")
            .contains("original_command=arbitrary-command")
    );
    fixture.finish();
}

#[test]
fn pinned_nix_completes_handshake_through_real_openssh_and_daemon() {
    let fixture = Fixture::start();
    let output = fixture
        .nix_command()
        .args([
            "--extra-experimental-features",
            "nix-command",
            "--store",
            "ssh-ng://telchar-openssh-test",
            "store",
            "info",
        ])
        .output()
        .expect("pinned Nix client runs through OpenSSH");

    assert!(output.status.success(), "pinned Nix failed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Version: telchar"),
        "server handshake missing from stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.finish();
}

struct Fixture {
    root: PathBuf,
    daemon: Child,
    sshd: Child,
    nix: PathBuf,
}

impl Fixture {
    fn start() -> Self {
        let root = std::env::temp_dir().join(format!(
            "telchar-openssh-integration-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir(&root).expect("fixture root creates");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("fixture root permissions set");
        let socket = root.join("daemon.sock");
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_telchar"));
        let mut daemon = Command::new(&binary)
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
        wait_for_path(&socket, &mut daemon);

        let ssh_keygen = command_path("ssh-keygen");
        let sshd = command_path("sshd");
        let nix = std::env::var_os("TELCHAR_NIX_BIN")
            .map(PathBuf::from)
            .expect("TELCHAR_NIX_BIN identifies flake-pinned Nix");
        let host_key = root.join("host-key");
        let client_key = root.join("client-key");
        run(
            &ssh_keygen,
            [
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-f",
                host_key.to_str().unwrap(),
            ],
        );
        run(
            &ssh_keygen,
            [
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-f",
                client_key.to_str().unwrap(),
            ],
        );
        let fingerprint = String::from_utf8(
            Command::new(&ssh_keygen)
                .args(["-lf", client_key.with_extension("pub").to_str().unwrap()])
                .output()
                .expect("fingerprint runs")
                .stdout,
        )
        .expect("fingerprint UTF-8")
        .split_whitespace()
        .nth(1)
        .expect("fingerprint exists")
        .to_owned();
        let port = 20000 + (unique_suffix() % 20000) as u16;
        let forced = root.join("forced-command.sh");
        fs::write(
            &forced,
            format!(
                "#!/bin/sh\nprintf 'original_command=%s\\n' \"${{SSH_ORIGINAL_COMMAND-}}\" > {}\nexec env TELCHAR_IPC_SOCKET={} TELCHAR_AUTHENTICATED_KEY={} {} serve-stdio\n",
                root.join("forced-command-output").display(),
                socket.display(),
                fingerprint,
                binary.display()
            ),
        )
        .expect("forced command writes");
        fs::set_permissions(&forced, fs::Permissions::from_mode(0o700))
            .expect("forced command permissions set");
        fs::write(
            root.join("authorized_keys"),
            format!(
                "command=\"{}\",no-pty,no-agent-forwarding,no-X11-forwarding,no-port-forwarding {}\n",
                forced.display(),
                fs::read_to_string(client_key.with_extension("pub")).expect("public key reads")
            ),
        )
        .expect("authorized keys writes");
        let config = root.join("sshd_config");
        fs::write(
            &config,
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\nStrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\nUsePAM no\nPermitUserEnvironment no\nAllowTcpForwarding no\nAllowAgentForwarding no\nX11Forwarding no\nPermitTTY no\nLogLevel ERROR\n",
                host_key.display(),
                root.join("sshd.pid").display(),
                root.join("authorized_keys").display()
            ),
        )
        .expect("sshd config writes");
        let sshd_log = root.join("sshd.log");
        let mut sshd_child = Command::new(&sshd)
            .args(["-D", "-e", "-f", config.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(fs::File::create(&sshd_log).expect("sshd log creates"))
            .spawn()
            .expect("sshd starts");
        wait_for_path(&root.join("sshd.pid"), &mut sshd_child);
        fs::write(
            root.join("ssh_config"),
            format!(
                "Host telchar-openssh-test\nHostName 127.0.0.1\nPort {port}\nUser {}\nIdentityFile {}\nIdentitiesOnly yes\nStrictHostKeyChecking no\nUserKnownHostsFile /dev/null\n",
                whoami(),
                client_key.display()
            ),
        )
        .expect("SSH config writes");
        Self {
            root,
            daemon,
            sshd: sshd_child,
            nix,
        }
    }

    fn ssh_command(&self) -> Command {
        let mut command = Command::new(command_path("ssh"));
        command.args([
            "-F",
            self.root
                .join("ssh_config")
                .to_str()
                .expect("UTF-8 SSH config"),
            "telchar-openssh-test",
        ]);
        command
    }

    fn nix_command(&self) -> Command {
        let mut command = Command::new(&self.nix);
        let ssh_directory = command_path("ssh")
            .parent()
            .expect("ssh has a parent directory")
            .to_owned();
        let path = format!(
            "{}:{}",
            ssh_directory.display(),
            std::env::var("PATH").expect("PATH exists")
        );
        command.env("PATH", path);
        command.env("HOME", &self.root);
        command.env(
            "NIX_SSHOPTS",
            format!("-F {}", self.root.join("ssh_config").display()),
        );
        command
    }

    fn finish(mut self) {
        self.sshd.kill().expect("sshd stops");
        self.sshd.wait().expect("sshd waits");
        self.daemon.kill().expect("daemon stops");
        self.daemon.wait().expect("daemon waits");
        fs::remove_dir_all(self.root).expect("fixture cleans up");
    }
}

fn command_path(name: &str) -> PathBuf {
    std::env::var_os(format!("{}_BIN", name.to_uppercase()))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(which(name)))
}

fn which(name: &str) -> String {
    Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .output()
        .expect("which runs")
        .stdout
        .iter()
        .map(|b| *b as char)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn run<const N: usize>(program: &Path, args: [&str; N]) {
    assert!(
        Command::new(program)
            .args(args)
            .status()
            .expect("command runs")
            .success()
    );
}

fn whoami() -> String {
    String::from_utf8(
        Command::new("id")
            .args(["-un"])
            .output()
            .expect("id runs")
            .stdout,
    )
    .expect("username UTF-8")
    .trim()
    .to_owned()
}

fn wait_for_path(path: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "fixture path did not appear: {path:?}"
        );
        assert!(
            child.try_wait().expect("child status").is_none(),
            "child exited before fixture was ready"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_nanos()
}
