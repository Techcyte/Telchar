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
    assert_eq!(
        read_word(&mut output),
        1,
        "authenticated ingress is trusted"
    );
    assert_eq!(read_word(&mut output), nix_worker_protocol::STDERR_LAST);

    drop(input);
    assert!(frontend.wait().expect("frontend exits").success());
    let result = fixture.daemon.wait_with_output().unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(!String::from_utf8_lossy(&result.stderr).contains("audit."));
    fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn frontend_delivers_response_after_request_half_close() {
    let root = tempfile::tempdir().expect("frontend fixture directory creates");
    let socket = root.path().join("daemon.sock");
    let listener =
        std::os::unix::net::UnixListener::bind(&socket).expect("fixture daemon socket binds");
    let mut frontend = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .arg("serve-stdio")
        .env("TELCHAR_IPC_SOCKET", &socket)
        .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("frontend starts");
    let daemon = thread::spawn(move || {
        let listener = IpcListener::from_listener(listener, rustix::process::getuid().as_raw());
        let mut connection = listener.accept().expect("authenticated envelope reads");
        let stream = connection.stream_mut();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("fixture daemon read timeout sets");
        let mut request = Vec::new();
        stream
            .read_to_end(&mut request)
            .expect("request reaches EOF");
        assert_eq!(request, b"request-marker");
        thread::sleep(Duration::from_millis(30));
        stream
            .write_all(b"response-marker")
            .expect("response writes");
    });
    frontend
        .stdin
        .take()
        .expect("frontend stdin")
        .write_all(b"request-marker")
        .expect("request writes before stdin closes");
    let output = wait_with_deadline(&mut frontend, Duration::from_secs(3));
    daemon.join().expect("fixture daemon completes");
    assert!(output.status.success(), "frontend failed: {output:?}");
    assert_eq!(output.stdout, b"response-marker");
    let diagnostics = String::from_utf8(output.stderr).expect("frontend diagnostics are UTF-8");
    let lines = diagnostics.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "unexpected diagnostics: {diagnostics}");
    for line in &lines {
        assert!(line.contains(" INFO "), "unexpected severity: {line}");
        assert!(
            line.contains("event=\"ipc.relay.completed\""),
            "unexpected event: {line}"
        );
        assert!(
            line.contains("reason=\"source-eof\""),
            "unexpected reason: {line}"
        );
        assert!(
            !line.contains("request-marker") && !line.contains("response-marker"),
            "payload leaked: {line}"
        );
    }
    for bytes in [14, 15] {
        assert!(
            lines
                .iter()
                .any(|line| line.contains(&format!("forwarded_bytes={bytes} "))),
            "missing byte count: {diagnostics}"
        );
    }
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
fn frontend_preserves_certificate_identity_and_mapping() {
    let fixture = Fixture::start();
    let config = fixture.root.join("certificate.toml");
    fs::write(
        &config,
        r#"
[identity.credentials."ssh-cert:9:SHA256:ca:7:builder"]
audit_subject = "release-engineering"
quota_subject = "shared-build-farm"
"#,
    )
    .expect("frontend configuration writes");
    let mut frontend = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .arg("serve-stdio")
        .env("TELCHAR_CONFIG", config)
        .env("TELCHAR_IPC_SOCKET", &fixture.socket)
        .env_remove("TELCHAR_AUTHENTICATED_KEY")
        .env("TELCHAR_AUTHENTICATED_CA", "SHA256:ca")
        .env("TELCHAR_AUTHENTICATED_KEY_ID", "builder")
        .env("TELCHAR_AUTHENTICATED_PRINCIPALS", "builder\nrelease")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("frontend starts");
    complete_handshake(&mut frontend);
    let sessions = fixture.database.connect().query(
        "SELECT credential_id, audit_subject, quota_subject, authentication_authority FROM protocol_sessions",
        &[],
    ).expect("sessions read");
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].get::<_, String>(0),
        "ssh-cert:9:SHA256:ca:7:builder"
    );
    assert_eq!(sessions[0].get::<_, String>(1), "release-engineering");
    assert_eq!(sessions[0].get::<_, String>(2), "shared-build-farm");
    assert_eq!(sessions[0].get::<_, String>(3), "openssh-certificate");
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
