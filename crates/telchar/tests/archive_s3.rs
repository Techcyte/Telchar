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
