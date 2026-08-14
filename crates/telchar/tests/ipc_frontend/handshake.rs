//! Tests IPC handshake.

use super::*;

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
fn frontend_applies_configured_identity_mapping() {
    let fixture = Fixture::start();
    let config = fixture.root.join("frontend.toml");
    fs::write(
        &config,
        r#"
[ipc]
socket = "/unused/by-environment-override.sock"

[identity.credentials."ssh-pubkey:SHA256:fixture"]
audit_subject = "release-engineering"
quota_subject = "shared-build-farm"
"#,
    )
    .expect("frontend configuration writes");
    let mut frontend = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .arg("serve-stdio")
        .env("TELCHAR_CONFIG", config)
        .env("TELCHAR_IPC_SOCKET", &fixture.socket)
        .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("frontend starts");

    let mut input = frontend.stdin.take().expect("frontend stdin");
    let mut output = frontend.stdout.take().expect("frontend stdout");
    complete_worker_handshake(&mut input, &mut output);
    drop(input);
    assert!(frontend.wait().expect("frontend exits").success());

    let sessions = fixture
        .database
        .connect()
        .query(
            "SELECT credential_id, audit_subject, quota_subject FROM protocol_sessions",
            &[],
        )
        .expect("sessions read");
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].get::<_, Option<String>>(0).as_deref(),
        Some("ssh-pubkey:SHA256:fixture")
    );
    assert_eq!(sessions[0].get::<_, String>(1), "release-engineering");
    assert_eq!(sessions[0].get::<_, String>(2), "shared-build-farm");
    fixture.finish_successfully();
}

#[test]
fn frontend_maps_multiple_credentials_to_one_quota_subject() {
    let fixture = Fixture::start_persistent(1_000);
    let config = fixture.root.join("shared-quota.toml");
    fs::write(
        &config,
        r#"
[identity.credentials."ssh-pubkey:SHA256:first"]
audit_subject = "first-owner"
quota_subject = "shared-build-farm"

[identity.credentials."ssh-pubkey:SHA256:second"]
audit_subject = "second-owner"
quota_subject = "shared-build-farm"
"#,
    )
    .expect("frontend configuration writes");

    for fingerprint in ["SHA256:first", "SHA256:second"] {
        let mut frontend = Command::new(env!("CARGO_BIN_EXE_telchar"))
            .arg("serve-stdio")
            .env("TELCHAR_CONFIG", &config)
            .env("TELCHAR_IPC_SOCKET", &fixture.socket)
            .env("TELCHAR_AUTHENTICATED_KEY", fingerprint)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("frontend starts");
        complete_handshake(&mut frontend);
    }

    let sessions = fixture
        .database
        .connect()
        .query(
            "SELECT credential_id, quota_subject FROM protocol_sessions ORDER BY credential_id",
            &[],
        )
        .expect("sessions read");
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions[0].get::<_, Option<String>>(0).as_deref(),
        Some("ssh-pubkey:SHA256:first")
    );
    assert_eq!(sessions[0].get::<_, String>(1), "shared-build-farm");
    assert_eq!(
        sessions[1].get::<_, Option<String>>(0).as_deref(),
        Some("ssh-pubkey:SHA256:second")
    );
    assert_eq!(sessions[1].get::<_, String>(1), "shared-build-farm");
    fixture.stop();
}

#[test]
fn frontend_uses_credential_id_as_unmapped_quota_subject() {
    let fixture = Fixture::start();
    let mut frontend = fixture.spawn_frontend();
    complete_handshake(&mut frontend);

    let sessions = fixture
        .database
        .connect()
        .query(
            "SELECT credential_id, quota_subject FROM protocol_sessions",
            &[],
        )
        .expect("sessions read");
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].get::<_, Option<String>>(0).as_deref(),
        Some("ssh-pubkey:SHA256:fixture")
    );
    assert_eq!(sessions[0].get::<_, String>(1), "ssh-pubkey:SHA256:fixture");
    fixture.finish_successfully();
}
