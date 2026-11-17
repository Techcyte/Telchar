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
    let config = root.path().join("telchar.toml");
    fs::write(&config, "[audit]\nenabled = true\n").unwrap();
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
            .env("TELCHAR_CONFIG", &config)
            .env("SSH_CONNECTION", "192.0.2.7 54321 192.0.2.8 22")
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
        let identity = envelope.identity_details.expect("audit identity supplied");
        assert_eq!(identity.source_address, Some("192.0.2.7".parse().unwrap()));
        assert_eq!(identity.source_port, Some(54321));
        assert_eq!(identity.key_type.as_deref(), Some("ssh-ed25519"));
        if file == "client.pub" {
            assert_eq!(identity.auth_method, "publickey");
            assert_eq!(identity.key_fingerprint, Some(fingerprint("client.pub")));
        } else {
            assert_eq!(identity.auth_method, "certificate");
            assert_eq!(
                identity.certificate_ca_fingerprint.as_deref(),
                Some(ca.as_str())
            );
            assert_eq!(identity.certificate_key_id.as_deref(), Some("build client"));
            assert_eq!(
                identity.certificate_principals,
                Some(vec!["builder".into(), "release".into()])
            );
            assert_eq!(identity.certificate_serial, Some(0));
            assert!(identity.certificate_valid_from.is_some());
            assert!(identity.certificate_valid_to.is_some());
            assert_eq!(
                identity.certificate_user_key_fingerprint,
                Some(fingerprint("client.pub"))
            );
        }
    }
}
