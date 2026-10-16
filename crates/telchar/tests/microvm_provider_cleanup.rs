//! Exercises exact provider identity through a real signed HTTP exchange.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

fn exchange(
    status: &str,
    body: &str,
    operation: &'static str,
) -> (Result<bool, telchar::microvm::ProviderError>, String) {
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    let control_url = fixture.url().to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback binds");
    listener.set_nonblocking(true).expect("bounded accept");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let server = std::thread::spawn(move || {
        let count = if operation == "lifecycle-cleanup" {
            3
        } else {
            1
        };
        let mut requests = String::new();
        for index in 0..count {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return String::new();
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("bounded read");
            let mut headers = Vec::new();
            let mut byte = [0];
            while !headers.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).expect("headers arrive");
                headers.push(byte[0]);
                assert!(headers.len() < 32768);
            }
            let response = if operation == "lifecycle-cleanup" && index == 1 {
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}".to_owned()
            } else if operation == "lifecycle-cleanup" && index == 2 {
                let body = observation("microvm-exact", "TERMINATED");
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else {
                response.clone()
            };
            if operation == "response-expired-cleanup" {
                postgres::Client::connect(&control_url, postgres::NoTls).unwrap().batch_execute("UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second' WHERE owner_kind = 'daemon'").unwrap();
            }
            socket
                .write_all(response.as_bytes())
                .expect("response sends");
            requests.push_str(&String::from_utf8(headers).expect("HTTP text"));
        }
        if operation == "response-expired-cleanup" {
            let until = std::time::Instant::now() + Duration::from_millis(300);
            while std::time::Instant::now() < until {
                match listener.accept() {
                    Ok(_) => panic!("ownership loss must prevent follow-up termination"),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            }
        }
        requests
    });
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-cleanup.drv";
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[1; 32],
        "cleanup-fixture",
        telchar::backend::BackendKind::Local,
        telchar::backend::BackendKind::Local.capabilities(),
        None,
        &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-cleanup"],
    )
    .expect("build claims");
    telchar::persistence::start_shared_build(fixture.url(), derivation).expect("attempt starts");
    let attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
        .expect("attempt reads")
        .expect("attempt exists");
    fixture.connect().execute(
        "INSERT INTO microvm_launches (intent_id, attempt_id, backend_name, account_id, region, image_arn, image_version, configuration_digest, request_digest, client_token, credential_verifier, admission_cutoff, maximum_duration_seconds, phase, provider_id) VALUES ('cleanup-intent', $1, 'cleanup-fixture', '116392764028', 'us-west-2', 'image-fixture', '1', $2, $2, 'cleanup-token', $2, clock_timestamp() + interval '1 minute', 600, 'closed', 'microvm-exact')",
        &[&attempt.attempt_id, &&[1_u8; 32][..]],
    ).expect("cleanup obligation persists");
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("owner acquires");
    if operation == "expired-cleanup" {
        fixture.expire_singleton_ownership("daemon");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let config = aws_sdk_lambdamicrovms::Config::builder()
        .behavior_version_latest()
        .region(aws_sdk_lambdamicrovms::config::Region::new(
            if operation == "region-cleanup" {
                "us-east-1"
            } else {
                "us-west-2"
            },
        ))
        .credentials_provider(aws_credential_types::Credentials::new(
            "fixture-key",
            "fixture-secret",
            None,
            None,
            "test",
        ))
        .endpoint_url(endpoint)
        .build();
    let provider =
        telchar::microvm::Provider::new(config, Duration::from_secs(1)).expect("provider");
    let result = if operation.ends_with("cleanup") {
        let cleanup =
            telchar::persistence::pending_microvm_cleanup(fixture.url(), "cleanup-fixture", 1)
                .expect("pending identity")
                .remove(0);
        let result = provider.cleanup(&owner, &cleanup);
        let released: bool = fixture.connect().query_one("SELECT reservation_released_at IS NOT NULL FROM microvm_launches WHERE intent_id = 'cleanup-intent'", &[]).expect("reservation reads").get(0);
        assert_eq!(released, result.as_ref().is_ok_and(|released| *released));
        if operation == "lifecycle-cleanup" {
            assert!(!result.expect("terminate acknowledgment retains capacity"));
            assert!(!released);
            let pending = telchar::persistence::pending_microvm_cleanup(
                owner.database_url(),
                "cleanup-fixture",
                1,
            )
            .unwrap();
            assert_eq!(pending.len(), 1);
            assert!(
                provider
                    .cleanup(&owner, &pending[0])
                    .expect("later exact terminal observation")
            );
            assert!(
                telchar::persistence::pending_microvm_cleanup(
                    owner.database_url(),
                    "cleanup-fixture",
                    1
                )
                .unwrap()
                .is_empty()
            );
            assert!(
                !provider.cleanup(&owner, &cleanup).unwrap(),
                "released obligation sends no further request"
            );
            Ok(true)
        } else {
            result
        }
    } else {
        runtime.block_on(async {
            if operation == "get" {
                provider.is_terminated("microvm-exact").await
            } else {
                provider.terminate("microvm-exact").await.map(|()| false)
            }
        })
    };
    (result, server.join().expect("server joins"))
}

fn observation(id: &str, state: &str) -> String {
    serde_json::json!({
        "microvmId": id, "state": state, "endpoint": "https://unused.invalid",
        "imageArn": "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture",
        "imageVersion": "1", "maximumDurationInSeconds": 600, "startedAt": 1,
        "terminatedAt": 2,
    })
    .to_string()
}

#[test]
fn exact_terminated_observation_is_required() {
    for state in ["RUNNING", "TERMINATING", "TERMINATED"] {
        let (result, request) = exchange("200 OK", &observation("microvm-exact", state), "get");
        assert_eq!(result.expect("observation decodes"), state == "TERMINATED");
        assert!(request.starts_with("GET /2025-09-09/microvms/microvm-exact "));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: aws4-hmac-sha256")
        );
    }
    let (result, _) = exchange("200 OK", &observation("microvm-other", "TERMINATED"), "get");
    assert!(result.is_err(), "different identity cannot prove cleanup");
    let (result, _) = exchange("404 Not Found", "{}", "get");
    assert!(result.is_err(), "absence does not prove cleanup");
}

#[test]
fn cleanup_releases_only_after_exact_provider_observation() {
    let (result, _) = exchange(
        "200 OK",
        &observation("microvm-exact", "TERMINATED"),
        "cleanup",
    );
    assert!(result.expect("exact terminal observation releases"));
    let (result, _) = exchange(
        "200 OK",
        &observation("microvm-other", "TERMINATED"),
        "cleanup",
    );
    assert!(result.is_err());
    let (result, _) = exchange("404 Not Found", "{}", "cleanup");
    assert!(result.is_err());
}

#[test]
fn cleanup_rejects_wrong_region_and_expired_owner_without_http_request() {
    for operation in ["region-cleanup", "expired-cleanup"] {
        let (result, request) = exchange(
            "200 OK",
            &observation("microvm-exact", "TERMINATED"),
            operation,
        );
        assert!(result.is_err(), "cleanup must reject {operation}");
        assert!(
            request.is_empty(),
            "cleanup must send no request: {operation}"
        );
    }
}

#[test]
fn cleanup_retains_capacity_until_poll_after_termination_request() {
    let (result, requests) = exchange(
        "200 OK",
        &observation("microvm-exact", "RUNNING"),
        "lifecycle-cleanup",
    );
    assert!(result.unwrap());
    let methods = requests
        .lines()
        .filter(|line| line.contains(" /2025-09-09/microvms/"))
        .collect::<Vec<_>>();
    assert_eq!(
        methods,
        [
            "GET /2025-09-09/microvms/microvm-exact HTTP/1.1",
            "DELETE /2025-09-09/microvms/microvm-exact HTTP/1.1",
            "GET /2025-09-09/microvms/microvm-exact HTTP/1.1",
        ]
    );
}

#[test]
fn ownership_loss_during_observation_prevents_termination_and_release() {
    let (result, requests) = exchange(
        "200 OK",
        &observation("microvm-exact", "RUNNING"),
        "response-expired-cleanup",
    );
    assert!(result.is_err());
    assert_eq!(
        requests
            .lines()
            .filter(|line| line.starts_with("GET "))
            .count(),
        1
    );
    assert!(!requests.contains("DELETE "));
}

#[test]
fn termination_request_does_not_prove_cleanup() {
    let (result, request) = exchange("200 OK", "{}", "terminate");
    assert!(!result.expect("termination accepted but not observed"));
    assert!(request.starts_with("DELETE /2025-09-09/microvms/microvm-exact "));
}
