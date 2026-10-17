use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[derive(Debug)]
struct Credentials {
    rotated: Arc<AtomicBool>,
}

impl aws_credential_types::provider::ProvideCredentials for Credentials {
    fn provide_credentials<'a>(
        &'a self,
    ) -> aws_credential_types::provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        aws_credential_types::provider::future::ProvideCredentials::new(async move {
            Ok(aws_credential_types::Credentials::new(
                if self.rotated.load(Ordering::SeqCst) {
                    "rotated-key"
                } else {
                    "initial-key"
                },
                "fixture-secret",
                None,
                None,
                "test",
            ))
        })
    }
}

#[test]
fn provider_reattests_rotated_credentials_before_cleanup() {
    rotation("116392764028");
    rotation("999999999999");
}

fn rotation(account: &'static str) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let until = std::time::Instant::now() + Duration::from_secs(5);
        let mut requests = Vec::new();
        while std::time::Instant::now() < until {
            let mut socket = match listener.accept() {
                Ok((socket, _)) => socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 32768);
            }
            let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .map(|value| value.parse::<usize>().unwrap())
                .unwrap_or(0);
            assert!(length < 4096);
            let mut body = vec![0; length];
            socket.read_exact(&mut body).unwrap();
            let identity = headers.starts_with("post ");
            let body = if identity {
                assert!(
                    String::from_utf8(body)
                        .unwrap()
                        .contains("Action=GetCallerIdentity")
                );
                let response = "<GetCallerIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><GetCallerIdentityResult><Account>116392764028</Account><Arn>arn:aws:iam::116392764028:user/fixture</Arn><UserId>fixture</UserId></GetCallerIdentityResult><ResponseMetadata><RequestId>fixture</RequestId></ResponseMetadata></GetCallerIdentityResponse>".to_owned();
                if headers.contains("credential=rotated-key/") {
                    response.replace("116392764028", account)
                } else {
                    response
                }
            } else {
                assert!(headers.starts_with("get "));
                serde_json::json!({
                    "microvmId": "microvm-exact", "state": "TERMINATED",
                    "endpoint": "https://unused.invalid", "imageArn": "image-fixture",
                    "imageVersion": "1", "maximumDurationInSeconds": 600,
                    "startedAt": 1, "terminatedAt": 2,
                })
                .to_string()
            };
            requests.push(headers);
            write!(socket, "HTTP/1.1 200 OK\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", if identity { "text/xml" } else { "application/json" }, body.len()).unwrap();
            if !identity {
                break;
            }
        }
        requests
    });
    let rotated = Arc::new(AtomicBool::new(false));
    let credentials = aws_credential_types::provider::SharedCredentialsProvider::new(Credentials {
        rotated: rotated.clone(),
    });
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
    let provider = runtime
        .block_on(telchar::microvm::Provider::verify_account(
            config,
            identity,
            credentials,
            "116392764028",
            Duration::from_secs(1),
        ))
        .unwrap();
    rotated.store(true, Ordering::SeqCst);
    let result = runtime.block_on(provider.is_terminated("microvm-exact"));
    if account == "116392764028" {
        assert!(result.unwrap());
    } else {
        assert!(
            result.is_err(),
            "foreign credentials cannot reach provider API"
        );
    }
    let requests = server.join().unwrap();
    assert_eq!(
        requests.len(),
        if account == "116392764028" { 3 } else { 2 },
        "only matching rotated authority permits provider operation"
    );
    assert!(requests[0].contains("credential=initial-key/"));
    assert!(requests[1].starts_with("post "));
    assert!(requests[1].contains("credential=rotated-key/"));
    if account == "116392764028" {
        assert!(requests[2].starts_with("get "));
        assert!(requests[2].contains("credential=rotated-key/"));
    }
}
