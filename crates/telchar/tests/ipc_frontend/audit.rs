//! Exercises opt-in session audit through the real frontend/daemon process boundary.

use super::*;

pub(super) fn audit_events(stderr: &str) -> Vec<serde_json::Value> {
    stderr
        .lines()
        .filter_map(|line| {
            let start = line.find('{')?;
            serde_json::from_str::<serde_json::Value>(&line[start..]).ok()
        })
        .collect()
}

#[test]
fn audit_session_reports_identity_protocol_and_summary() {
    audit_session(false);
    audit_session(true);
}

fn audit_session(certificate: bool) {
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let database = PostgresFixture::start();
    let mut command = daemon_command(&socket, 5000, true, database.url());
    let config = socket.with_extension("toml");
    let mut contents = fs::read_to_string(&config).unwrap();
    contents.push_str("\n[audit]\nenabled = true\n");
    fs::write(&config, contents).unwrap();
    command.env("RUST_LOG", "off");
    let mut daemon = command.spawn().unwrap();
    wait_for_socket(&socket, &mut daemon);
    let mut frontend = Command::new(env!("CARGO_BIN_EXE_telchar"));
    if certificate {
        frontend
            .env_remove("TELCHAR_AUTHENTICATED_KEY")
            .env("TELCHAR_AUTHENTICATED_CA", "SHA256:audit-ca")
            .env("TELCHAR_AUTHENTICATED_KEY_ID", "alice-cert")
            .env("TELCHAR_AUTHENTICATED_PRINCIPALS", "alice\nteam")
            .env("TELCHAR_AUTHENTICATED_SERIAL", "42")
            .env("TELCHAR_AUTHENTICATED_VALID_FROM", "2026-01-01T00:00:00Z")
            .env("TELCHAR_AUTHENTICATED_VALID_TO", "2027-01-01T00:00:00Z")
            .env("TELCHAR_AUTHENTICATED_USER_KEY", "SHA256:user-key");
    } else {
        frontend
            .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:audit-key")
            .env_remove("TELCHAR_AUTHENTICATED_CA");
    }
    let mut frontend = frontend
        .arg("serve-stdio")
        .env("TELCHAR_CONFIG", &config)
        .env("TELCHAR_IPC_SOCKET", &socket)
        .env("TELCHAR_AUTHENTICATED_KEY_TYPE", "ssh-ed25519")
        .env("SSH_CONNECTION", "192.0.2.1 54321 192.0.2.2 22")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = frontend.stdin.take().unwrap();
    let mut output = frontend.stdout.take().unwrap();
    complete_worker_handshake(&mut input, &mut output);
    let target = b"/nix/store/00000000000000000000000000000000-missing.drv!*";
    write_word(&mut input, 46);
    write_word(&mut input, 1);
    write_word(&mut input, target.len() as u64);
    input.write_all(target).unwrap();
    input
        .write_all(&vec![0; (8 - target.len() % 8) % 8])
        .unwrap();
    write_word(&mut input, 0);
    input.flush().unwrap();
    assert_eq!(read_word(&mut output), nix_worker_protocol::STDERR_ERROR);
    drop(input);
    assert!(frontend.wait_with_output().unwrap().status.success());
    let output = daemon.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "daemon failed after protocol rejection: {output:?}"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let events = audit_events(&stderr);
    let started = events
        .iter()
        .find(|event| event["event"] == "session.started")
        .expect("session.started");
    if certificate {
        assert_eq!(started["audit_subject"], "alice");
        assert_eq!(started["identity"]["certificate_serial"], 42);
        assert_eq!(
            started["identity"]["certificate_user_key_fingerprint"],
            "SHA256:user-key"
        );
        assert_eq!(
            started["identity"]["certificate_principals"],
            serde_json::json!(["alice", "team"])
        );
    } else {
        assert_eq!(started["credential_id"], "ssh-pubkey:SHA256:audit-key");
        assert_eq!(started["audit_subject"], "SHA256:audit-key");
    }
    assert_eq!(started["identity"]["key_type"], "ssh-ed25519");
    assert_eq!(started["identity"]["source_port"], 54321);
    assert!(started["client_protocol"].is_string());
    let ended = events
        .iter()
        .find(|event| event["event"] == "session.ended")
        .expect("session.ended");
    assert_eq!(ended["session_id"], started["session_id"]);
    assert_eq!(ended["credential_id"], started["credential_id"]);
    assert_eq!(ended["audit_subject"], started["audit_subject"]);
    assert_eq!(ended["paths_uploaded"], 0);
    assert_eq!(ended["builds_requested"], 1);
    let requested = events
        .iter()
        .find(|event| event["event"] == "build.requested")
        .expect("build.requested");
    let completed = events
        .iter()
        .find(|event| event["event"] == "build.completed")
        .expect("build.completed");
    assert_eq!(requested["session_id"], started["session_id"]);
    assert_eq!(completed["status"], "failed");
    fs::remove_dir_all(root).unwrap();
}
