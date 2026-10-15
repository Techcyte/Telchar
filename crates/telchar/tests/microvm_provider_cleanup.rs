//! Exercises exact provider identity through a real signed HTTP exchange.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

fn exchange(
    status: &str,
    body: &str,
    operation: &str,
) -> (Result<bool, telchar::microvm::ProviderError>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback binds");
    listener.set_nonblocking(true).expect("bounded accept");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "request arrives before deadline"
                    );
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
        socket
            .write_all(response.as_bytes())
            .expect("response sends");
        String::from_utf8(headers).expect("HTTP text")
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = runtime.block_on(async {
        let config = aws_sdk_lambdamicrovms::Config::builder()
            .behavior_version_latest()
            .region(aws_sdk_lambdamicrovms::config::Region::new("us-west-2"))
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
        if operation == "get" {
            provider.is_terminated("microvm-exact").await
        } else {
            provider.terminate("microvm-exact").await.map(|()| false)
        }
    });
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
fn termination_request_does_not_prove_cleanup() {
    let (result, request) = exchange("200 OK", "{}", "terminate");
    assert!(!result.expect("termination accepted but not observed"));
    assert!(request.starts_with("DELETE /2025-09-09/microvms/microvm-exact "));
}
