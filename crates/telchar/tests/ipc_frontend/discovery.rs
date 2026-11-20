//! Tests admission while preparing discovered SSH members.

use super::*;

#[test]
fn discovered_inventory_probe_does_not_block_worker_handshake() {
    let root = temporary_root();
    let socket = root.join("discovery.sock");
    let database = PostgresFixture::start();
    let mut command = daemon_command(&socket, 1_000, false, database.url());
    command.env(
        "TELCHAR_GATEWAY_STORE_URI",
        "unix:///nix/var/nix/daemon-socket/socket",
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (publish, publication) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = [0; 4096];
        assert!(stream.read(&mut request).unwrap() > 0);
        publication.recv_timeout(Duration::from_secs(5)).unwrap();
        let body = r#"[{"Node":{"Node":"node","Address":"127.0.0.1"},"Service":{"ID":"builder","Service":"builder","Address":"127.0.0.1","Port":22},"Checks":[{"Status":"passing"}]}]"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
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
            "#!/bin/sh\nprintf probing > '{}'\nsleep 2\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    let config = socket.with_extension("toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{contents}\n[[backends.ssh]]\ncheck_timeout_seconds = 3\nidentity_file = \"{}\"\nknown_hosts_file = \"{}\"\nssh_program = \"{}\"\n[backends.ssh.discovered]\nsource = \"consul\"\nendpoint = \"http://{address}\"\nservice = \"builder\"\nrefresh_interval_seconds = 300\nrequest_timeout_seconds = 5\n", identity.display(), hosts.display(), ssh.display())).unwrap();
    let mut daemon = command.spawn().unwrap();
    wait_for_socket(&socket, &mut daemon);
    publish.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "discovery probe did not start");
        assert!(daemon.try_wait().unwrap().is_none());
        thread::sleep(Duration::from_millis(5));
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
            session_id: "during-discovery".into(),
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
    daemon.kill().unwrap();
    let output = daemon.wait_with_output().unwrap();
    server.join().unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(
        handshake.is_ok(),
        "discovery blocked admission: {handshake:?}; {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(u64::from_le_bytes(response), SERVER_WORKER_MAGIC);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("configuration.reload.completed"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("configuration.reload.rejected"),
        "{stderr}"
    );
}
