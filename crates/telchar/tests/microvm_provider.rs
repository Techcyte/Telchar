//! Exercises the signed provider client against a real loopback HTTP boundary.

mod support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

#[test]
fn reserved_launch_records_one_dispatch_and_quarantines_provider_failure() {
    launch_exchange(false, "active");
    launch_exchange(true, "active");
    launch_exchange(true, "expired");
    launch_exchange(true, "response-expired");
}

fn launch_exchange(success: bool, ownership: &'static str) {
    let fixture = support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-provider.drv";
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[1; 32],
        "provider-fixture",
        telchar::backend::BackendKind::Local,
        telchar::backend::BackendKind::Local.capabilities(),
        None,
        &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-provider"],
    )
    .expect("fixture build claims");
    telchar::persistence::start_shared_build(fixture.url(), derivation).expect("attempt starts");
    let attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
        .expect("attempt reads")
        .expect("attempt exists");
    let intent = telchar::persistence::MicrovmLaunchIntent {
        intent_id: "intent-fixture".to_owned(),
        attempt_id: attempt.attempt_id,
        backend_name: "provider-fixture".to_owned(),
        account_id: "116392764028".to_owned(),
        region: "us-west-2".to_owned(),
        image_arn: "image-fixture".to_owned(),
        image_version: "1".to_owned(),
        configuration_digest: [1; 32],
        request_digest: [2; 32],
        client_token: "intent-fixture".to_owned(),
        credential_verifier: [3; 32],
        admission_cutoff: std::time::SystemTime::now() + Duration::from_secs(60),
        maximum_duration_seconds: 600,
    };
    assert!(
        telchar::persistence::reserve_microvm_launch(fixture.url(), &intent, 1)
            .expect("capacity reserves")
    );
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("owner acquires");
    if ownership == "expired" {
        fixture.expire_singleton_ownership("daemon");
    }
    let control_url = fixture.url().to_owned();
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
                    if ownership == "response-expired" {
                        postgres::Client::connect(&control_url, postgres::NoTls).expect("fixture control connects").batch_execute("UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second' WHERE owner_kind = 'daemon'").expect("owner expires during request");
                    }
                    let (status, body) = if success {
                        ("200 OK", serde_json::json!({
                            "microvmId": "microvm-exact", "state": "PENDING", "endpoint": "https://unused.invalid",
                            "imageArn": "image-fixture", "imageVersion": "1", "maximumDurationInSeconds": 600, "startedAt": 1,
                        }).to_string())
                    } else {
                        ("500 Internal Server Error", "{}".to_owned())
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket
                        .write_all(response.as_bytes())
                        .expect("provider response");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        }
        requests
    });
    {
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
        let result = provider.launch_reserved(&owner, &intent, "{}");
        if ownership != "active" {
            assert!(result.is_err(), "expired owner cannot dispatch or bind");
        } else if success {
            assert_eq!(
                result.expect("launch binds"),
                Some("microvm-exact".to_owned())
            );
        } else {
            assert_eq!(
                result.expect_err("500 is ambiguous").to_string(),
                "MicroVM provider operation failed"
            );
        }
        let repeat = provider.launch_reserved(&owner, &intent, "{}");
        if ownership == "active" {
            assert_eq!(repeat.expect("repeat has no dispatch grant"), None);
        } else {
            assert!(repeat.is_err());
        }
    }
    let requests = server.join().expect("server joins");
    assert_eq!(requests.len(), if ownership == "expired" { 0 } else { 1 });
    if ownership != "expired" {
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
    let row = fixture.connect().query_one(
        "SELECT phase, provider_id, reservation_released_at IS NULL FROM microvm_launches WHERE intent_id = 'intent-fixture'", &[],
    ).expect("durable launch reads");
    assert_eq!(
        row.get::<_, String>(0),
        match ownership {
            "expired" => "prepared",
            "response-expired" => "dispatching",
            _ if success => "bound",
            _ => "ambiguous",
        }
    );
    assert_eq!(
        row.get::<_, Option<String>>(1).as_deref(),
        if success && ownership == "active" {
            Some("microvm-exact")
        } else {
            None
        }
    );
    assert!(row.get::<_, bool>(2));
}
