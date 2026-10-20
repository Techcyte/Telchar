use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

#[path = "microvm_identity/rotation.rs"]
mod rotation;

#[test]
fn provider_requires_matching_signed_account_identity() {
    for account in ["116392764028", "999999999999"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < until);
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 32768);
            }
            let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            assert!(headers.contains("authorization: aws4-hmac-sha256 credential=fixture-key/"));
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            assert!(length < 4096);
            let mut body = vec![0; length];
            socket.read_exact(&mut body).unwrap();
            assert!(
                String::from_utf8(body)
                    .unwrap()
                    .contains("Action=GetCallerIdentity")
            );
            let body = format!(
                "<GetCallerIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><GetCallerIdentityResult><Account>{account}</Account><Arn>arn:aws:iam::{account}:user/fixture</Arn><UserId>fixture</UserId></GetCallerIdentityResult><ResponseMetadata><RequestId>fixture</RequestId></ResponseMetadata></GetCallerIdentityResponse>"
            );
            write!(socket, "HTTP/1.1 200 OK\r\ncontent-type: text/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
            drop(socket);
            if account == "116392764028" {
                for returned in [
                    "exact",
                    "wrong-id",
                    "wrong-image",
                    "wrong-version",
                    "wrong-id",
                    "running",
                    "terminal",
                    "terminal",
                    "terminal",
                ] {
                    let until = std::time::Instant::now() + Duration::from_secs(5);
                    let mut socket = loop {
                        match listener.accept() {
                            Ok((socket, _)) => break socket,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(
                                    std::time::Instant::now() < until,
                                    "provider request after runtime shutdown"
                                );
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            Err(error) => panic!("{error}"),
                        }
                    };
                    socket
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut headers = Vec::new();
                    while !headers.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        socket.read_exact(&mut byte).unwrap();
                        headers.push(byte[0]);
                        assert!(headers.len() < 32768);
                    }
                    let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
                    assert!(headers.contains("credential=fixture-key/"));
                    assert!(headers.starts_with("get "));
                    assert!(headers.contains("microvm-exact"));
                    let body = serde_json::json!({
                        "microvmId": if returned == "wrong-id" { "microvm-other" } else { "microvm-exact" },
                        "state": if returned == "running" { "RUNNING" } else { "TERMINATED" },
                        "endpoint": "https://unused.invalid",
                        "imageArn": if returned == "wrong-image" { "other-image" } else { "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture" },
                        "imageVersion": if returned == "wrong-version" { "2" } else { "1" },
                        "maximumDurationInSeconds": 600,
                        "startedAt": 1, "terminatedAt": 2,
                    })
                    .to_string();
                    write!(socket, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            }
        });
        let credentials = aws_credential_types::Credentials::new(
            "fixture-key",
            "fixture-secret",
            None,
            None,
            "test",
        );
        let identity = aws_sdk_sts::Config::builder()
            .behavior_version_latest()
            .region(aws_sdk_sts::config::Region::new("us-west-2"))
            .credentials_provider(credentials.clone())
            .endpoint_url(endpoint.clone())
            .build();
        let config = aws_sdk_lambdamicrovms::Config::builder()
            .behavior_version_latest()
            .region(aws_sdk_lambdamicrovms::config::Region::new("us-west-2"))
            .credentials_provider(credentials.clone())
            .endpoint_url(endpoint)
            .build();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let unattested =
            telchar::microvm::Provider::new(config.clone(), Duration::from_secs(2)).unwrap();
        assert!(
            runtime
                .block_on(unattested.verify_identity(
                    "microvm-exact",
                    "116392764028",
                    "us-west-2",
                    "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture",
                    "1",
                ))
                .is_err()
        );
        assert!(
            runtime
                .block_on(unattested.verify_termination(
                    "microvm-exact",
                    "116392764028",
                    "us-west-2"
                ))
                .is_err()
        );
        let result = runtime.block_on(telchar::microvm::Provider::verify_account(
            config,
            identity,
            aws_credential_types::provider::SharedCredentialsProvider::new(credentials),
            "116392764028",
            Duration::from_secs(2),
        ));
        assert_eq!(result.is_ok(), account == "116392764028");
        drop(runtime);
        if let Ok(provider) = result {
            for (expected_account, region) in
                [("999999999999", "us-west-2"), ("116392764028", "us-east-1")]
            {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                assert!(
                    runtime
                        .block_on(provider.verify_termination(
                            "microvm-exact",
                            expected_account,
                            region
                        ))
                        .is_err()
                );
                assert!(
                    runtime
                        .block_on(provider.verify_identity(
                            "microvm-exact",
                            expected_account,
                            region,
                            "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture",
                            "1",
                        ))
                        .is_err()
                );
            }
            for expected_match in [true, false, false, false] {
                let provider = provider.clone();
                std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    assert_eq!(
                        runtime
                            .block_on(provider.verify_identity(
                                "microvm-exact",
                                "116392764028",
                                "us-west-2",
                                "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture",
                                "1",
                            ))
                            .is_ok(),
                        expected_match
                    );
                })
                .join()
                .unwrap();
            }
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            for expected_match in [false, false, true] {
                assert_eq!(
                    runtime
                        .block_on(provider.verify_termination(
                            "microvm-exact",
                            "116392764028",
                            "us-west-2"
                        ))
                        .is_ok(),
                    expected_match
                );
            }
            drop(runtime);
            for _ in 0..2 {
                let provider = provider.clone();
                std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    assert!(
                        runtime
                            .block_on(provider.is_terminated("microvm-exact"))
                            .unwrap()
                    );
                })
                .join()
                .unwrap();
            }
        }
        server.join().unwrap();
    }
}
