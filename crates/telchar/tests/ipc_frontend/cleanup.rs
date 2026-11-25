//! Tests daemon admission and durable root cleanup across filesystem faults.

use super::*;

#[test]
fn daemon_defers_root_cleanup_and_recovers_after_permission_repair() {
    let root = temporary_root();
    let socket = root.join("cleanup.sock");
    let database = PostgresFixture::start();
    let mut command = daemon_command(&socket, 1_000, false, database.url());
    let roots = root.join("roots");
    fs::create_dir(&roots).unwrap();
    fs::set_permissions(&roots, fs::Permissions::from_mode(0o700)).unwrap();
    command
        .env("TELCHAR_TEST_MAINTENANCE_INTERVAL_MS", "100")
        .env(
            "TELCHAR_GATEWAY_STORE_URI",
            "unix:///nix/var/nix/daemon-socket/socket",
        )
        .env("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY", &roots);
    let diagnostic_path = root.join("daemon.stderr");
    command.stderr(Stdio::from(fs::File::create(&diagnostic_path).unwrap()));
    let mut daemon = command.spawn().unwrap();
    wait_for_socket(&socket, &mut daemon);
    database.connect().batch_execute("INSERT INTO build_requests (request_id,derivation_path,system) VALUES ('cleanup-request','/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-cleanup.drv','x86_64-linux'); INSERT INTO store_leases (lease_id,owner_kind,owner_id,store_path,purpose,state,nar_size,created_at,released_at) VALUES ('released','request','cleanup-request','/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-output','input','released',1,now(),now())").unwrap();
    std::os::unix::fs::symlink(
        "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-output",
        roots.join("released"),
    )
    .unwrap();
    fs::set_permissions(&roots, fs::Permissions::from_mode(0o500)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let diagnostic = fs::read_to_string(&diagnostic_path).unwrap();
        if diagnostic.contains("gateway.root_cleanup.deferred") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cleanup fault was not observed: {diagnostic}"
        );
        thread::sleep(Duration::from_millis(100));
    }
    let alive = daemon.try_wait().unwrap().is_none();
    let pending = telchar::persistence::read_store_lease(database.url(), "released")
        .unwrap()
        .unwrap()
        .state;
    let protected = fs::symlink_metadata(roots.join("released")).is_ok();
    let handshake = if alive {
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        IpcListener::send_envelope(&mut stream, &authenticated_envelope("during-cleanup-fault"))
            .unwrap();
        write_word(&mut stream, CLIENT_WORKER_MAGIC);
        write_word(&mut stream, LATEST_WORKER_VERSION.to_wire());
        write_word(&mut stream, 0);
        stream.flush().unwrap();
        let mut response = [0; 8];
        let result = stream.read_exact(&mut response);
        if result.is_ok() {
            assert_eq!(u64::from_le_bytes(response), SERVER_WORKER_MAGIC);
            assert_eq!(read_word(&mut stream), LATEST_WORKER_VERSION.to_wire());
            assert_eq!(read_word(&mut stream), 0);
            complete_post_handshake_stream(&mut stream);
        }
        result
    } else {
        Err(std::io::Error::other("daemon exited during cleanup fault"))
    };
    fs::set_permissions(&roots, fs::Permissions::from_mode(0o700)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut recovered = false;
    while alive && Instant::now() < deadline {
        if daemon.try_wait().unwrap().is_some() {
            break;
        }
        if telchar::persistence::read_store_lease(database.url(), "released")
            .unwrap()
            .unwrap()
            .state
            == telchar::persistence::StoreLeaseState::Reconciled
        {
            recovered = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if recovered {
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        IpcListener::send_envelope(&mut stream, &authenticated_envelope("after-cleanup-repair"))
            .unwrap();
        let mut output = stream.try_clone().unwrap();
        complete_worker_handshake(&mut stream, &mut output);
    }
    if daemon.try_wait().unwrap().is_none() {
        daemon.kill().unwrap();
    }
    let output = daemon.wait_with_output().unwrap();
    assert!(output.stderr.is_empty());
    let stderr = fs::read_to_string(&diagnostic_path).unwrap();
    let removed = fs::symlink_metadata(roots.join("released")).is_err();
    fs::remove_dir_all(root).unwrap();
    assert!(alive, "{stderr}");
    assert_eq!(pending, telchar::persistence::StoreLeaseState::Released);
    assert!(protected);
    assert!(
        matches!(
            handshake.as_ref().err().map(std::io::Error::kind),
            Some(std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::UnexpectedEof)
        ),
        "{handshake:?}; {stderr}"
    );
    assert!(
        stderr.contains("diagnostic=store root cleanup pending"),
        "{stderr}"
    );
    assert!(recovered && removed, "{stderr}");
    assert!(stderr.contains("gateway.root_cleanup.deferred"), "{stderr}");
    assert!(!stderr.contains("maintenance failed"), "{stderr}");
    assert!(output.stdout.is_empty());
}
