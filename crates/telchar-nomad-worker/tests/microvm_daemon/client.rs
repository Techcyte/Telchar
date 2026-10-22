use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(super) fn build(root: &Path, socket: &Path, derivation: &[u8]) -> io::Result<()> {
    let home = root.join("client");
    std::fs::create_dir(&home)?;
    let empty = home.join("empty");
    std::fs::create_dir(&empty)?;
    std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o755))?;
    let result = Command::new("mount")
        .arg("--bind")
        .arg(&empty)
        .arg("/var/empty")
        .output()?;
    assert!(result.status.success(), "{result:?}");
    assert!(result.stderr.is_empty(), "{result:?}");
    let key = home.join("key");
    let result = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .output()?;
    assert!(result.status.success(), "{result:?}");
    assert!(result.stderr.is_empty(), "{result:?}");
    let binary = std::env::var_os("TELCHAR_TEST_DAEMON_BIN").unwrap();
    let forced = home.join("serve");
    std::fs::write(
        &forced,
        format!(
            "#!/bin/sh\nexport TELCHAR_IPC_SOCKET='{}' TELCHAR_AUTHENTICATED_KEY='SHA256:microvm-client'\nexec '{}' serve-stdio\n",
            socket.display(),
            Path::new(&binary).display()
        ),
    )?;
    std::fs::set_permissions(&forced, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        home.join("authorized_keys"),
        format!(
            "restrict,command=\"{}\" {}",
            forced.display(),
            std::fs::read_to_string(key.with_extension("pub"))?
        ),
    )?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let config = home.join("sshd_config");
    std::fs::write(
        &config,
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\nStrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nPermitRootLogin yes\nAllowTcpForwarding no\nPermitTTY no\nLogLevel ERROR\n",
            key.display(),
            home.join("sshd.pid").display(),
            home.join("authorized_keys").display()
        ),
    )?;
    let sshd = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("sshd"))
        .find(|path| path.is_file())
        .expect("sshd available");
    drop(listener);
    let mut server = super::Daemon(
        Command::new("unshare")
            .args(["--user", "--map-user=1000", "--map-group=1000"])
            .arg(sshd)
            .args(["-D", "-e", "-f"])
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(home.join("sshd.log"))?)
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !home.join("sshd.pid").exists() {
        assert!(
            server.0.try_wait()?.is_none(),
            "sshd exited: {}",
            std::fs::read_to_string(home.join("sshd.log"))?
        );
        assert!(Instant::now() < deadline, "sshd startup deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
    let user = Command::new("id").args(["-un", "1000"]).output()?;
    assert!(user.status.success());
    let user = String::from_utf8(user.stdout).unwrap();
    let result = Command::new(std::env::var_os("TELCHAR_NIX_BIN").unwrap())
        .env("HOME", &home)
        .env("NIX_SSHOPTS", format!("-p {port} -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o BatchMode=yes", key.display()))
        .args(["--extra-experimental-features", "nix-command", "--store"])
        .arg(format!("ssh-ng://{}@127.0.0.1", user.trim()))
        .args(["build", "--no-link", "--print-out-paths"])
        .arg(format!("{}^*", std::str::from_utf8(derivation).unwrap()))
        .output()?;
    assert!(
        result.status.success(),
        "stock Nix: {result:?}; sshd: {}",
        std::fs::read_to_string(home.join("sshd.log"))?
    );
    assert!(result.stdout.is_empty(), "{result:?}");
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        format!(
            "this derivation will be built:\n  {}\n",
            std::str::from_utf8(derivation).unwrap()
        )
    );
    Ok(())
}
