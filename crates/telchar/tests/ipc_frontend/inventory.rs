//! Tests daemon admission while preparing an SSH inventory generation.

use super::*;

#[test]
fn delayed_reload_probe_does_not_block_worker_handshake() {
    reload_during_admission(false);
}

#[test]
fn shutdown_reaps_pending_inventory_probe() {
    reload_during_admission(true);
}

fn reload_during_admission(shutdown: bool) {
    let root = temporary_root();
    let socket = root.join("inventory.sock");
    let database = PostgresFixture::start();
    let mut command = daemon_command(&socket, 1_000, false, database.url());
    command.env(
        "TELCHAR_GATEWAY_STORE_URI",
        "unix:///nix/var/nix/daemon-socket/socket",
    );
    let mut daemon = command.spawn().unwrap();
    wait_for_socket(&socket, &mut daemon);
    let identity = root.join("identity");
    let hosts = root.join("known-hosts");
    let ssh = root.join("ssh");
    let marker = root.join("probing");
    fs::write(&identity, "fixture-key").unwrap();
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&hosts, "builder ssh-ed25519 AAAA\n").unwrap();
    fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nprintf '%s' $$ > '{}.pending'\nmv '{}.pending' '{}'\nsleep {}\nexit 1\n",
            marker.display(),
            marker.display(),
            marker.display(),
            if shutdown { 10 } else { 2 }
        ),
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    let config = socket.with_extension("toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{contents}\n[[backends.ssh]]\ncheck_timeout_seconds = 15\nidentity_file = \"{}\"\nknown_hosts_file = \"{}\"\nssh_program = \"{}\"\n[backends.ssh.pool]\nsource = \"static\"\n[backends.ssh.pool.delayed]\naddress = \"builder\"\n", identity.display(), hosts.display(), ssh.display())).unwrap();
    assert!(
        Command::new("kill")
            .args(["-HUP", &daemon.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "reload probe did not start");
        assert!(
            daemon.try_wait().unwrap().is_none(),
            "daemon exited during reload"
        );
        thread::sleep(Duration::from_millis(5));
    }
    if shutdown {
        let pid: u32 = fs::read_to_string(&marker).unwrap().parse().unwrap();
        assert!(
            Command::new("kill")
                .args(["-TERM", &daemon.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        while daemon.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "daemon shutdown did not finish");
            thread::sleep(Duration::from_millis(10));
        }
        let probe_alive = std::path::Path::new(&format!("/proc/{pid}")).exists();
        let output = daemon.wait_with_output().unwrap();
        thread::sleep(Duration::from_secs(11));
        fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !probe_alive,
            "daemon exited before pending probe {pid} was reaped"
        );
        return;
    }
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    IpcListener::send_envelope(
        &mut stream,
        &IpcEnvelope {
            version: IPC_VERSION,
            requester: RequesterMetadata {
                credential_id: "ssh-pubkey:fixture".into(),
                audit_subject: "fixture".into(),
                quota_subject: "fixture".into(),
            },
            session_id: "during-inventory".into(),
            trace_context: telchar_telemetry::TraceContext::default(),
            identity_details: None,
            error: None,
        },
    )
    .unwrap();
    write_word(&mut stream, CLIENT_WORKER_MAGIC);
    write_word(&mut stream, LATEST_WORKER_VERSION.to_wire());
    write_word(&mut stream, 0);
    stream.flush().unwrap();
    let mut response = [0; 8];
    let handshake = stream.read_exact(&mut response);
    if handshake.is_ok() {
        assert_eq!(read_word(&mut stream), LATEST_WORKER_VERSION.to_wire());
        assert_eq!(read_word(&mut stream), 0);
        complete_post_handshake_stream(&mut stream);
    }
    drop(stream);
    thread::sleep(Duration::from_secs(3));
    fs::remove_file(&marker).unwrap();
    assert!(
        Command::new("kill")
            .args(["-HUP", &daemon.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "second reload did not start");
        assert!(daemon.try_wait().unwrap().is_none());
        thread::sleep(Duration::from_millis(5));
    }
    let mut second = UnixStream::connect(&socket).unwrap();
    second
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    IpcListener::send_envelope(
        &mut second,
        &IpcEnvelope {
            version: IPC_VERSION,
            requester: RequesterMetadata {
                credential_id: "ssh-pubkey:fixture".into(),
                audit_subject: "fixture".into(),
                quota_subject: "fixture".into(),
            },
            session_id: "during-second-inventory".into(),
            trace_context: telchar_telemetry::TraceContext::default(),
            identity_details: None,
            error: None,
        },
    )
    .unwrap();
    write_word(&mut second, CLIENT_WORKER_MAGIC);
    write_word(&mut second, LATEST_WORKER_VERSION.to_wire());
    write_word(&mut second, 0);
    second.flush().unwrap();
    let mut second_response = [0; 8];
    let second_handshake = second.read_exact(&mut second_response);
    if second_handshake.is_ok() {
        assert_eq!(read_word(&mut second), LATEST_WORKER_VERSION.to_wire());
        assert_eq!(read_word(&mut second), 0);
        complete_post_handshake_stream(&mut second);
    }
    drop(second);
    thread::sleep(Duration::from_secs(3));
    daemon.kill().unwrap();
    let output = daemon.wait_with_output().unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(
        handshake.is_ok(),
        "admission blocked during probe: {handshake:?}; {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(u64::from_le_bytes(response), SERVER_WORKER_MAGIC);
    assert!(
        second_handshake.is_ok(),
        "second admission blocked: {second_handshake:?}"
    );
    assert_eq!(u64::from_le_bytes(second_response), SERVER_WORKER_MAGIC);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("configuration.reload.rejected"),
        "{stderr}"
    );
    assert!(
        stderr.matches("configuration.reload.completed").count() == 2,
        "{stderr}"
    );
}
