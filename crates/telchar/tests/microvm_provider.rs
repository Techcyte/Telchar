//! Exercises the signed provider client against a real loopback HTTP boundary.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

#[test]
fn failed_launch_has_one_signed_http_attempt() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback binds");
    listener.set_nonblocking(true).expect("listener bounded");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let server = std::thread::spawn(move || {
        let until = std::time::Instant::now() + Duration::from_secs(3);
        let mut requests = Vec::new();
        while std::time::Instant::now() < until {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .expect("read bounded");
                    let mut request = Vec::new();
                    let mut byte = [0];
                    while !request.ends_with(b"\r\n\r\n") {
                        socket.read_exact(&mut byte).expect("HTTP headers");
                        request.push(byte[0]);
                        assert!(request.len() < 32768);
                    }
                    let headers = String::from_utf8(request).expect("HTTP text");
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().expect("body length"))
                        })
                        .expect("bounded body");
                    assert!(length < 32768);
                    let mut body = vec![0; length];
                    socket.read_exact(&mut body).expect("request body");
                    requests.push((headers, body));
                    socket.write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}").expect("fault response");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        }
        requests
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
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
        let error = provider
            .launch("image-fixture", "1", "intent-fixture", "{}", 600)
            .await
            .expect_err("500 is ambiguous");
        assert_eq!(error.to_string(), "MicroVM provider operation failed");
    });
    let requests = server.join().expect("server joins");
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .0
            .to_ascii_lowercase()
            .contains("authorization: aws4-hmac-sha256")
    );
    let body: serde_json::Value = serde_json::from_slice(&requests[0].1).expect("launch body");
    assert_eq!(body["clientToken"], "intent-fixture");
    assert_eq!(body["imageVersion"], "1");
    assert_eq!(body["maximumDurationInSeconds"], 600);
    assert_eq!(body["idlePolicy"]["autoResumeEnabled"], false);
}
