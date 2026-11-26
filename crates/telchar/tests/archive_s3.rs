//! Tests signed archive delivery against a simulated S3 HTTP boundary.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use telchar::service::archive::S3Archive;

fn verify_signature(request: &str, body: &[u8]) {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::{Digest, Sha256};
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
    fn hmac(key: &[u8], value: &str) -> Vec<u8> {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(value.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
    let headers: std::collections::BTreeMap<_, _> = request
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let authorization = &headers["authorization"];
    let fields: std::collections::BTreeMap<_, _> = authorization
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .unwrap()
        .split(", ")
        .map(|field| field.split_once('=').unwrap())
        .collect();
    let credential = fields["Credential"];
    let scope = credential.strip_prefix("fixture/").unwrap();
    let parts: Vec<_> = scope.split('/').collect();
    assert_eq!(&parts[1..], &["us-east-1", "s3", "aws4_request"]);
    let signed = fields["SignedHeaders"];
    let canonical_headers: String = signed
        .split(';')
        .map(|name| format!("{name}:{}\n", headers[name]))
        .collect();
    let target = request
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    let canonical = format!(
        "PUT\n{target}\n\n{canonical_headers}\n{signed}\n{}",
        hex(&Sha256::digest(body))
    );
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        headers["x-amz-date"],
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let key = hmac(b"AWS4private", parts[0]);
    let key = hmac(&key, parts[1]);
    let key = hmac(&key, parts[2]);
    let key = hmac(&key, parts[3]);
    assert_eq!(hex(&hmac(&key, &to_sign)), fields["Signature"]);
}

#[test]
fn s3_delivery_signs_immutable_put_and_rejects_storage_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/bucket/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for status in ["200 OK", "503 Service Unavailable"] {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
                if bytes.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let request_text = String::from_utf8(bytes).unwrap();
            let request = request_text.to_ascii_lowercase();
            assert!(request.starts_with("put /bucket/history/42.jsonl http/1.1"));
            assert!(request.contains("authorization: aws4-hmac-sha256"));
            assert!(request.contains("if-none-match: *"));
            let mut body = [0; 8];
            socket.read_exact(&mut body).unwrap();
            assert_eq!(&body, b"history\n");
            verify_signature(&request_text, &body);
            write!(
                socket,
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        }
    });
    let credentials =
        aws_credential_types::Credentials::new("fixture", "private", None, None, "test");
    let archive =
        S3Archive::new(&endpoint, "us-east-1", credentials, Duration::from_secs(5)).unwrap();
    archive.deliver("history/42.jsonl", b"history\n").unwrap();
    assert!(archive.deliver("history/42.jsonl", b"history\n").is_err());
    server.join().unwrap();
}

#[test]
fn s3_delivery_rejects_redirects_and_bounds_unresponsive_storage() {
    for timeout in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let redirected = TcpListener::bind("127.0.0.1:0").unwrap();
        redirected.set_nonblocking(true).unwrap();
        let location = format!("http://{}/credentials", redirected.local_addr().unwrap());
        let endpoint = format!("http://{}/bucket/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 16384);
            }
            let mut body = [0; 8];
            socket.read_exact(&mut body).unwrap();
            if timeout {
                std::thread::sleep(Duration::from_millis(400));
            } else {
                write!(socket, "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
        });
        let credentials =
            aws_credential_types::Credentials::new("fixture", "private", None, None, "test");
        let archive = S3Archive::new(
            &endpoint,
            "us-east-1",
            credentials,
            Duration::from_millis(100),
        )
        .unwrap();
        let started = std::time::Instant::now();
        let error = archive
            .deliver("history/42.jsonl", b"history\n")
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!error.to_string().contains("private"));
        server.join().unwrap();
        assert_eq!(
            redirected.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn archive_credentials_reject_empty_fields_without_contacting_storage() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let credentials_file = root.path().join("credentials");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let policy = telchar::service::config::HistoryS3 {
        endpoint: format!("http://{}/bucket/", listener.local_addr().unwrap()),
        region: "us-east-1".to_owned(),
        credentials_file: credentials_file.clone(),
        timeout: Duration::from_millis(100),
    };
    for document in [
        r#"{"access_key_id":"","secret_access_key":"sensitive"}"#,
        r#"{"access_key_id":"fixture","secret_access_key":""}"#,
        r#"{"access_key_id":"fixture","secret_access_key":"sensitive","session_token":""}"#,
    ] {
        std::fs::write(&credentials_file, document).unwrap();
        std::fs::set_permissions(&credentials_file, std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let error = telchar::service::archive::deliver_archive(
            None,
            Some(&policy),
            "history/test.jsonl",
            b"record\n",
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "invalid archive credentials file");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn s3_existing_objects_require_identical_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/bucket/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for body in [b"history\n".as_slice(), b"conflict".as_slice()] {
            for method in ["PUT", "GET"] {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                }
                assert!(String::from_utf8(headers).unwrap().starts_with(method));
                if method == "PUT" {
                    let mut payload = [0; 8];
                    socket.read_exact(&mut payload).unwrap();
                    socket.write_all(b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                } else {
                    write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    socket.write_all(body).unwrap();
                }
            }
        }
    });
    let archive = S3Archive::new(
        &endpoint,
        "us-east-1",
        aws_credential_types::Credentials::new("fixture", "private", None, None, "test"),
        Duration::from_secs(5),
    )
    .unwrap();
    archive.deliver("history/42.jsonl", b"history\n").unwrap();
    assert_eq!(
        archive
            .deliver("history/42.jsonl", b"history\n")
            .unwrap_err()
            .to_string(),
        "archive content conflict"
    );
    server.join().unwrap();
}

#[test]
fn log_delivery_retries_identical_compressed_bytes_after_storage_outage() {
    use std::os::unix::fs::PermissionsExt;
    use telchar::backend::BackendKind;
    let database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let drv = "/nix/store/77777777777777777777777777777777-s3-log.drv";
    telchar::persistence::claim_shared_build(
        database.url(),
        drv,
        &[7; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/88888888888888888888888888888888-output"],
    )
    .unwrap();
    telchar::persistence::start_shared_build(database.url(), drv).unwrap();
    let attempt = telchar::persistence::read_shared_build_attempt(database.url(), drv)
        .unwrap()
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/bucket/", listener.local_addr().unwrap());
    let attempt_id = attempt.attempt_id;
    let server = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for status in ["503 Service Unavailable", "200 OK"] {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 16384);
            }
            let request = String::from_utf8(headers).unwrap();
            assert!(request.starts_with(&format!("PUT /bucket/logs/{attempt_id}.log.zst ")));
            let size: usize = request
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .unwrap()
                .parse()
                .unwrap();
            assert!(size < 4096);
            let mut body = vec![0; size];
            socket.read_exact(&mut body).unwrap();
            verify_signature(&request, &body);
            bodies.push(body);
            write!(
                socket,
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        }
        assert_eq!(bodies[0], bodies[1]);
        assert_eq!(
            zstd::stream::decode_all(bodies[1].as_slice()).unwrap(),
            b"archived over signed transport"
        );
    });
    let root = tempfile::tempdir().unwrap();
    let spool = root.path().join("spool");
    std::fs::create_dir(&spool).unwrap();
    let credentials = root.path().join("credentials.json");
    std::fs::write(
        &credentials,
        br#"{"access_key_id":"fixture","secret_access_key":"private"}"#,
    )
    .unwrap();
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config_file = root.path().join("service.toml");
    std::fs::write(&config_file, format!("[log_retention]\nretention_days = 7\ndirectory = '{}'\nmaximum_bytes = 1024\nmaximum_queue_bytes = 1024\nmaximum_captures = 1\nmaximum_spool_bytes = 67584\n[log_retention.s3]\nendpoint = '{endpoint}'\nregion = 'us-east-1'\ncredentials_file = '{}'\n", spool.display(), credentials.display())).unwrap();
    let config =
        telchar::service::config::ServiceConfig::load_with_environment(&config_file, |_| None)
            .unwrap();
    let policy = config.log_retention().unwrap();
    let manager = telchar::service::archive::AttemptLogs::new(spool.clone(), 1024, 1024, 1, 67584)
        .with_destination(None, policy.s3.clone());
    let connection = telchar::persistence::Database::connect(database.url()).unwrap();
    let capture = manager.begin(&connection, attempt_id).unwrap();
    capture.push(b"archived over signed transport");
    capture.finish();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let state: String = database
            .connect()
            .query_one("SELECT state FROM attempt_log_archives", &[])
            .unwrap()
            .get(0);
        if state == "pending" {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let local = spool.join(format!("{attempt_id}.log.zst"));
    assert!(local.exists());
    manager.recover(&connection).unwrap();
    let row = database
        .connect()
        .query_one(
            "SELECT state, complete, object_key FROM attempt_log_archives",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "available");
    assert!(row.get::<_, bool>(1));
    assert!(
        row.get::<_, String>(2)
            .ends_with(&format!("logs/{attempt_id}.log.zst"))
    );
    assert!(!local.exists());
    server.join().unwrap();
}

#[test]
fn configured_s3_history_requires_acknowledgement_before_deletion() {
    use std::os::unix::fs::PermissionsExt;
    use telchar::backend::BackendKind;
    let database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let drv = "/nix/store/77777777777777777777777777777777-s3-history.drv";
    telchar::persistence::claim_shared_build(
        database.url(),
        drv,
        &[7; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/88888888888888888888888888888888-output"],
    )
    .unwrap();
    telchar::persistence::start_shared_build(database.url(), drv).unwrap();
    telchar::persistence::complete_shared_build_failure(
        database.url(),
        drv,
        "build-failure",
        &serde_json::json!({"status":"failed"}),
        Duration::from_secs(1),
    )
    .unwrap();
    database.connect().batch_execute("UPDATE shared_builds SET created_at = now() - interval '3 days', started_at = now() - interval '3 days', completed_at = now() - interval '2 days', expires_at = now() - interval '1 day'; UPDATE shared_build_attempts SET created_at = now() - interval '3 days', started_at = now() - interval '3 days', completed_at = now() - interval '2 days'").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/bucket/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for status in ["503 Service Unavailable", "200 OK"] {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
            }
            let request = String::from_utf8(bytes).unwrap();
            let size: usize = request
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; size];
            socket.read_exact(&mut body).unwrap();
            verify_signature(&request, &body);
            bodies.push(body);
            write!(
                socket,
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        }
        assert_eq!(bodies[0], bodies[1]);
        serde_json::from_slice::<serde_json::Value>(&bodies[1]).unwrap()
    });
    let root = tempfile::tempdir().unwrap();
    let credentials = root.path().join("credentials.json");
    std::fs::write(
        &credentials,
        br#"{"access_key_id":"fixture","secret_access_key":"private"}"#,
    )
    .unwrap();
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config_file = root.path().join("service.toml");
    std::fs::write(&config_file, format!("[history_retention]\nretention_days = 1\nmaximum_builds = 1\n[history_retention.s3]\nendpoint = '{endpoint}'\nregion = 'us-east-1'\ncredentials_file = '{}'\n", credentials.display())).unwrap();
    let config =
        telchar::service::config::ServiceConfig::load_with_environment(&config_file, |_| None)
            .unwrap();
    let mut cursor = telchar::persistence::HistoryCursor::default();
    assert!(
        telchar::service::archive::maintain_history(
            database.url(),
            config.history_retention(),
            &mut cursor
        )
        .is_err()
    );
    assert!(
        telchar::persistence::read_shared_build(database.url(), drv)
            .unwrap()
            .is_some()
    );
    // Cursor wraps without bypassing the failed object's acknowledgement.
    assert_eq!(
        telchar::service::archive::maintain_history(
            database.url(),
            config.history_retention(),
            &mut cursor
        )
        .unwrap(),
        0
    );
    assert_eq!(
        telchar::service::archive::maintain_history(
            database.url(),
            config.history_retention(),
            &mut cursor
        )
        .unwrap(),
        1
    );
    assert!(
        telchar::persistence::read_shared_build(database.url(), drv)
            .unwrap()
            .is_none()
    );
    assert_eq!(server.join().unwrap()["build"]["derivation_path"], drv);
}
