use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

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
            .endpoint_url(endpoint)
            .build();
        let config = aws_sdk_lambdamicrovms::Config::builder()
            .behavior_version_latest()
            .region(aws_sdk_lambdamicrovms::config::Region::new("us-west-2"))
            .credentials_provider(credentials.clone())
            .endpoint_url("http://127.0.0.1:1")
            .build();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(telchar::microvm::Provider::verify_account(
            config,
            identity,
            aws_credential_types::provider::SharedCredentialsProvider::new(credentials),
            "116392764028",
            Duration::from_secs(2),
        ));
        assert_eq!(result.is_ok(), account == "116392764028");
        server.join().unwrap();
    }
}
