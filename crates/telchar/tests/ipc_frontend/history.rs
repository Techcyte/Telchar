//! Tests configured daemon maintenance against aged lease details.

use super::*;

#[test]
fn daemon_maintains_configured_detail_retention() {
    let root = temporary_root();
    let socket = root.join("history.sock");
    let database = PostgresFixture::start();
    let mut command = daemon_command(&socket, 1_000, false, database.url());
    let config = socket.with_extension("toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!("lifecycle_detail_retention_days = 1\n{contents}"),
    )
    .unwrap();
    command.env("TELCHAR_TEST_MAINTENANCE_INTERVAL_MS", "100");
    command.env(
        "TELCHAR_GATEWAY_STORE_URI",
        "unix:///nix/var/nix/daemon-socket/socket",
    );
    let mut daemon = command.spawn().unwrap();
    wait_for_socket(&socket, &mut daemon);
    database.connect().batch_execute("
        INSERT INTO protocol_sessions (session_id, requester_reference, state, created_at, closed_at)
        VALUES ('history-daemon', 'fixture', 'closed', now() - interval '4 days', now() - interval '3 days');
        INSERT INTO store_leases (lease_id,owner_kind,owner_id,store_path,purpose,state,nar_size,created_at,released_at,reconciled_at)
        VALUES ('aged-daemon','session','history-daemon','/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-input','input','reconciled',1,
            now() - interval '4 days',now() - interval '3 days',now() - interval '2 days'),
            ('recent-daemon','session','history-daemon','/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-input','input','reconciled',1,
            now() - interval '4 days',now() - interval '3 days',now());
        INSERT INTO protocol_sessions (session_id,requester_reference,state,created_at,closed_at)
        VALUES ('history-attachment','fixture','closed',now() - interval '4 days',now() - interval '3 days');
        INSERT INTO build_requests (request_id,derivation_path,system)
        VALUES ('history-request','/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-history.drv','x86_64-linux');
        INSERT INTO request_attachments (session_id,request_id,state,attached_at,detached_at)
        VALUES ('history-attachment','history-request','detached',now() - interval '4 days',now() - interval '3 days');
    ").unwrap();
    daemon.kill().unwrap();
    let predecessor = daemon.wait_with_output().unwrap();
    assert!(predecessor.stdout.is_empty());
    let stderr = String::from_utf8(predecessor.stderr).unwrap();
    assert!(!stderr.contains("maintenance failed"), "{stderr}");
    fs::remove_file(&socket).unwrap();
    thread::sleep(Duration::from_secs(4));
    let mut daemon = command.spawn().unwrap();
    wait_for_socket(&socket, &mut daemon);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = daemon.try_wait().unwrap() {
            let output = daemon.wait_with_output().unwrap();
            panic!(
                "daemon exited {status}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let count: i64 = database
            .connect()
            .query_one(
                "SELECT (SELECT count(*) FROM store_leases WHERE lease_id = 'aged-daemon') + (SELECT count(*) FROM protocol_sessions WHERE session_id = 'history-attachment') + (SELECT count(*) FROM request_attachments WHERE request_id = 'history-request')",
                &[],
            )
            .unwrap()
            .get(0);
        if count == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon maintenance did not prune eligible detail"
        );
        thread::sleep(Duration::from_millis(100));
    }
    let mut client = database.connect();
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM store_leases WHERE lease_id = 'recent-daemon'",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM build_requests WHERE request_id = 'history-request'",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    daemon.kill().unwrap();
    let output = daemon.wait_with_output().unwrap();
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("maintenance failed"), "{stderr}");
    fs::remove_dir_all(root).unwrap();
}
