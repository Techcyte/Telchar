//! Real gateway-store imports produce per-path audit decisions.

use super::*;
use std::os::unix::net::UnixStream;

fn word(output: &mut Vec<u8>, value: u64) {
    nix_worker_protocol::write_worker_integer(output, value);
}

fn string(output: &mut Vec<u8>, value: &[u8]) {
    nix_worker_protocol::write_worker_byte_string(output, value);
}

#[test]
fn uploads_report_acceptance_rejection_and_existing_paths() {
    assert!(
        std::env::var_os("TELCHAR_STORE_ROOT").is_some(),
        "isolated canonical store required"
    );
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let database = PostgresFixture::start();
    let mut command = daemon_command(&socket, 5000, false, database.url());
    let config = socket.with_extension("toml");
    let mut contents = fs::read_to_string(&config).unwrap();
    contents.push_str("\n[audit]\nenabled = true\n");
    fs::write(&config, contents).unwrap();
    command.env(
        "TELCHAR_GATEWAY_STORE_URI",
        "unix:///nix/var/nix/daemon-socket/socket",
    );
    let mut daemon = command.spawn().unwrap();
    wait_for_socket(&socket, &mut daemon);
    let path = format!(
        "/nix/store/11111111111111111111111111111111-audit-{}",
        std::process::id()
    );
    let mut nar = Vec::new();
    for value in [
        b"nix-archive-1".as_slice(),
        b"(",
        b"type",
        b"regular",
        b"contents",
        b"audit-content",
        b")",
    ] {
        string(&mut nar, value);
    }
    // Nix computes the NAR hash independently from the bytes sent to Telchar.
    let source = root.join("nar");
    fs::write(&source, &nar).unwrap();
    let output = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "hash",
            "file",
            "--type",
            "sha256",
            "--base16",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let hash = String::from_utf8(output.stdout).unwrap();
    for (index, rejected) in [false, false, true].into_iter().enumerate() {
        let mut stream = UnixStream::connect(&socket).unwrap();
        let envelope = authenticated_envelope(&format!("upload-audit-{index}"));
        IpcListener::send_envelope(&mut stream, &envelope).unwrap();
        let client =
            nix_worker_protocol::WorkerClient::connect(stream.try_clone().unwrap()).unwrap();
        let mut logical = Vec::new();
        word(&mut logical, 1);
        string(&mut logical, path.as_bytes());
        string(&mut logical, b"");
        string(
            &mut logical,
            if rejected {
                b"0000000000000000000000000000000000000000000000000000000000000000"
            } else {
                hash.trim().as_bytes()
            },
        );
        word(&mut logical, 0);
        word(&mut logical, 0);
        word(&mut logical, nar.len() as u64);
        word(&mut logical, 0);
        word(&mut logical, 1);
        string(&mut logical, b"host-key:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==");
        string(&mut logical, b"");
        logical.extend_from_slice(&nar);
        let mut wire = Vec::new();
        for value in [44, 0, 1, logical.len() as u64] {
            word(&mut wire, value);
        }
        wire.extend_from_slice(&logical);
        word(&mut wire, 0);
        stream.write_all(&wire).unwrap();
        stream.flush().unwrap();
        assert_eq!(
            read_word(&mut stream),
            if rejected {
                nix_worker_protocol::STDERR_ERROR
            } else {
                nix_worker_protocol::STDERR_LAST
            }
        );
        if index == 1 {
            let mut client = client;
            client
                .build_paths_with_results(&[path.as_bytes().to_vec()])
                .unwrap();
        } else {
            drop(client);
        }
        drop(stream);
    }
    thread::sleep(Duration::from_millis(1200));
    daemon.kill().unwrap();
    let output = daemon.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let events = super::audit::audit_events(&stderr);
    let uploads = events
        .iter()
        .filter(|event| event["store_path"] == path)
        .collect::<Vec<_>>();
    assert_eq!(uploads.len(), 3, "{stderr}");
    assert_eq!(uploads[0]["event"], "upload.accepted");
    assert_eq!(uploads[0]["already_valid"], false);
    assert_eq!(uploads[1]["event"], "upload.accepted");
    assert_eq!(uploads[1]["already_valid"], true);
    assert_eq!(uploads[2]["event"], "upload.rejected");
    assert!(
        uploads[2]["reason"]
            .as_str()
            .unwrap()
            .contains("NAR hash mismatch")
    );
    let completed = events
        .iter()
        .find(|event| event["event"] == "build.completed")
        .expect("already-valid build audit");
    assert_eq!(completed["status"], "already-valid");
    assert_eq!(completed["output_paths"], serde_json::json!([path]));
    for event in uploads {
        assert!(event["session_id"].is_string());
        assert_eq!(event["audit_subject"], "fixture");
        assert_eq!(event["credential_id"], "ssh-pubkey:fixture");
        assert_eq!(
            event["signatures"],
            serde_json::json!([{"key_name":"host-key","value":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}])
        );
    }
    fs::remove_dir_all(root).unwrap();
}
