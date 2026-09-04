//! Verifies authenticated OpenSSH identities through the packaged forced command and frontend.

use std::fs;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use telchar::service::ipc::IpcListener;

#[test]
fn forced_command_preserves_public_key_and_certificate_identities() {
    let root = tempfile::tempdir().expect("fixture directory");
    for name in ["ca", "client"] {
        let output = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(root.path().join(name))
            .output()
            .expect("key generation runs");
        assert!(output.status.success(), "{output:?}");
    }
    let output = Command::new("ssh-keygen")
        .args(["-q", "-s"])
        .arg(root.path().join("ca"))
        .args(["-I", "build client", "-n", "builder,release", "-V", "+1h"])
        .arg(root.path().join("client.pub"))
        .output()
        .expect("certificate signing runs");
    assert!(output.status.success(), "{output:?}");
    let fingerprint = |name: &str| {
        let output = Command::new("ssh-keygen")
            .args(["-lf"])
            .arg(root.path().join(name))
            .output()
            .expect("fingerprint runs");
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout)
            .expect("fingerprint text")
            .split_whitespace()
            .nth(1)
            .expect("fingerprint field")
            .to_owned()
    };
    let ca = fingerprint("ca.pub");
    for (file, expected_id, audit) in [
        (
            "client.pub",
            format!("ssh-pubkey:{}", fingerprint("client.pub")),
            fingerprint("client.pub"),
        ),
        (
            "client-cert.pub",
            format!("ssh-cert:{}:{ca}:12:build client", ca.len()),
            "builder".to_owned(),
        ),
    ] {
        let auth = root.path().join("auth");
        fs::write(
            &auth,
            format!(
                "publickey {}",
                fs::read_to_string(root.path().join(file)).unwrap()
            ),
        )
        .unwrap();
        let socket = root.path().join(format!("{file}.sock"));
        let listener = UnixListener::bind(&socket).expect("listener binds");
        listener.set_nonblocking(true).unwrap();
        let listener = IpcListener::from_listener(listener, rustix::process::getuid().as_raw());
        let mut child = Command::new("bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../deploy/ssh/telchar-ssh-forced-command.sh"
            ))
            .env("SSH_USER_AUTH", &auth)
            .env("TELCHAR_PROGRAM", env!("CARGO_BIN_EXE_telchar"))
            .env("TELCHAR_IPC_SOCKET", &socket)
            .env("TELCHAR_AUTHENTICATED_KEY", "untrusted")
            .env("TELCHAR_AUTHENTICATED_CA", "untrusted")
            .env("TELCHAR_AUTHENTICATED_KEY_ID", "untrusted")
            .env("TELCHAR_AUTHENTICATED_PRINCIPALS", "untrusted")
            .env_remove("TELCHAR_CONFIG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("forced command starts");
        let deadline = Instant::now() + Duration::from_secs(5);
        let connection = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                        let _ = child.kill();
                        let output = child.wait_with_output().unwrap();
                        panic!("forced command did not connect: {output:?}");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        let envelope = connection.envelope().clone();
        drop(connection);
        let output = child.wait_with_output().expect("frontend exits");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(envelope.requester.credential_id, expected_id);
        assert_eq!(envelope.requester.audit_subject, audit);
        assert_eq!(envelope.requester.quota_subject, expected_id);
    }
}
