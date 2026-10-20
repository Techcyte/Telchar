//! Checks the administrative socket's kernel identity and bounded transport.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use telchar::service::admin::{Request, read_request};

#[test]
fn administrative_requests_reject_claimed_actor_and_frontend_envelopes() {
    for bytes in [
        br#"{"action":"risk-release","intent_id":"intent","provider_id":null,"reason":"incident","evidence_reference":"ticket:1","accept_remote_execution_risk":true,"actor_uid":0}"#.as_slice(),
        b"TIPC\x02\x00".as_slice(),
    ] {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
        client.write_all(bytes).unwrap();
        assert!(read_request(&mut server, Duration::from_secs(1)).is_err());
    }
}

#[test]
fn administrative_frames_have_size_and_absolute_time_bounds() {
    let (mut client, mut server) = UnixStream::pair().unwrap();
    client.write_all(&u32::MAX.to_le_bytes()).unwrap();
    assert!(read_request(&mut server, Duration::from_secs(1)).is_err());

    let (_client, mut server) = UnixStream::pair().unwrap();
    let start = Instant::now();
    assert!(read_request(&mut server, Duration::from_millis(50)).is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn administrative_request_round_trip_preserves_exact_identity() {
    let request = Request::RiskRelease {
        intent_id: "intent".into(),
        provider_id: Some("microvm-exact".into()),
        reason: "Remote execution risk accepted".into(),
        evidence_reference: "incident:119".into(),
        accept_remote_execution_risk: true,
    };
    let (mut client, mut server) = UnixStream::pair().unwrap();
    telchar::service::admin::write_request(&mut client, &request).unwrap();
    assert_eq!(
        read_request(&mut server, Duration::from_secs(1)).unwrap(),
        request
    );
}

#[test]
fn trickled_bytes_do_not_extend_administrative_deadline() {
    let (mut client, mut server) = UnixStream::pair().unwrap();
    client.write_all(&100_u32.to_le_bytes()).unwrap();
    let writer = std::thread::spawn(move || {
        for _ in 0..20 {
            if client.write_all(b" ").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let start = Instant::now();
    assert!(read_request(&mut server, Duration::from_millis(100)).is_err());
    assert!(start.elapsed() < Duration::from_millis(300));
    drop(server);
    writer.join().unwrap();
}

#[test]
fn cli_requires_explicit_acknowledgment_and_rejects_duplicate_arguments() {
    for (arguments, diagnostic) in [
        (
            vec!["admin", "microvm", "risk-release"],
            "explicit remote execution risk acknowledgment required",
        ),
        (
            vec![
                "admin",
                "microvm",
                "risk-release",
                "--intent",
                "one",
                "--intent",
                "two",
            ],
            "duplicate administrative argument",
        ),
        (
            vec!["admin", "microvm", "risk-release", "--actor-uid", "0"],
            "unknown administrative argument",
        ),
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_telchar"))
            .env_clear()
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!("telchar: {diagnostic}\n")
        );
    }
}

#[test]
fn cli_sends_exact_request_and_reports_noop_without_database_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("admin.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_telchar"))
        .env_clear()
        .args(["admin", "microvm", "risk-release", "--socket"])
        .arg(&socket)
        .args([
            "--intent",
            "intent",
            "--provider-id",
            "microvm-exact",
            "--reason",
            "Remote execution risk accepted",
            "--evidence",
            "incident:120",
            "--accept-remote-execution-risk",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "CLI did not connect");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error}"),
        }
    };
    assert_eq!(
        read_request(&mut stream, Duration::from_secs(1)).unwrap(),
        Request::RiskRelease {
            intent_id: "intent".into(),
            provider_id: Some("microvm-exact".into()),
            reason: "Remote execution risk accepted".into(),
            evidence_reference: "incident:120".into(),
            accept_remote_execution_risk: true,
        }
    );
    let response = br#"{"released":false,"attached":false,"rejected":false}"#;
    stream
        .write_all(&(response.len() as u32).to_le_bytes())
        .unwrap();
    stream.write_all(response).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"released\":false,\"attached\":false,\"rejected\":false}\n"
    );
}

#[test]
fn cli_attachment_requires_exact_provider_identity() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_telchar"))
        .env_clear()
        .args([
            "admin",
            "microvm",
            "attach-identity",
            "--socket",
            "/unused",
            "--intent",
            "intent",
            "--reason",
            "Recovered response",
            "--evidence",
            "incident:131",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "telchar: --provider-id required\n"
    );
}

#[test]
fn nonroot_peer_cannot_reach_administrative_request_decoding() {
    assert_ne!(unsafe { libc::geteuid() }, 0, "run fixture as nonroot");
    let (_client, server) = UnixStream::pair().unwrap();
    let error = telchar::service::admin::authorize(&server).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
}
