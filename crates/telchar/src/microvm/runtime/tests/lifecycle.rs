use super::*;
use std::io::{Read, Write};
use std::time::Duration;

fn request(stream: &mut impl Read) -> String {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
        assert!(headers.len() < 32768);
    }
    let headers = String::from_utf8(headers).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(|s| s.parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    headers + &String::from_utf8(body).unwrap()
}

fn reply(stream: &mut impl Write, status: u16, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();
}

#[test]
fn attested_provider_reset_retains_worker_and_delivers_next_assignment() {
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    crate::persistence::migrate(fixture.url()).unwrap();
    let owner = std::sync::Arc::new(
        crate::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(60),
        )
        .unwrap(),
    );
    let database = owner.database();
    let path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-runtime.drv";
    let kind = crate::backend::BackendKind::Microvm;
    crate::persistence::claim_shared_build(
        database,
        path,
        &[1; 32],
        "arm",
        kind,
        kind.capabilities(),
        None,
        &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-output"],
    )
    .unwrap();
    crate::persistence::start_shared_build(database, path).unwrap();
    let attempt = crate::persistence::read_shared_build_attempt(database, path)
        .unwrap()
        .unwrap()
        .attempt_id;
    let intent = crate::persistence::MicrovmLaunchIntent {
        intent_id: "runtime-intent".into(),
        attempt_id: attempt,
        backend_name: "arm".into(),
        account_id: "116392764028".into(),
        region: "us-west-2".into(),
        image_arn: "image-fixture".into(),
        image_version: "1".into(),
        vpc_egress_connector_arn: "connector".into(),
        configuration_digest: [1; 32],
        request_digest: [2; 32],
        client_token: "runtime-intent".into(),
        credential_verifier: [3; 32],
        admission_cutoff: std::time::SystemTime::now() + Duration::from_secs(60),
        maximum_duration_seconds: 600,
    };
    assert!(crate::persistence::reserve_microvm_launch(database, &intent, 1).unwrap());
    assert!(crate::persistence::dispatch_microvm_launch(database, &intent.intent_id).unwrap());
    assert!(
        crate::persistence::bind_microvm_launch(database, &intent.intent_id, "microvm-exact")
            .unwrap()
    );
    crate::persistence::collect_shared_build(database, path).unwrap();
    crate::persistence::complete_shared_build_success(
        database,
        path,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();

    let tls = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let runtime_endpoint = format!("https://localhost:{}", tls.local_addr().unwrap().port());
    let certificate =
        include_bytes!("../../../../../telchar-nomad-worker/tests/fixtures/microvm-cert.der");
    let key = include_bytes!("../../../../../telchar-nomad-worker/tests/fixtures/microvm-key.der");
    let runtime_server = std::thread::spawn(move || {
        let config = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.to_vec().into()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.to_vec().into()),
        )
        .unwrap();
        for run in [false, true] {
            let (stream, _) = tls.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut stream = rustls::StreamOwned::new(
                rustls::ServerConnection::new(std::sync::Arc::new(config.clone())).unwrap(),
                stream,
            );
            let incoming = request(&mut stream);
            assert!(
                incoming
                    .to_ascii_lowercase()
                    .contains("x-aws-proxy-auth: fixture-token\r\n")
            );
            assert!(
                incoming
                    .to_ascii_lowercase()
                    .contains("x-aws-proxy-port: 8080\r\n")
            );
            if run {
                assert!(incoming.starts_with("POST /aws/lambda-microvms/runtime/v1/run "));
                let payload: serde_json::Value =
                    serde_json::from_str(incoming.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                assert_eq!(payload["assignment_epoch"], 2);
                assert_eq!(payload["intent_id"], "runtime-intent");
                reply(&mut stream, 200, "{}");
            } else {
                assert!(incoming.starts_with("POST /aws/lambda-microvms/runtime/v1/validate "));
                reply(
                    &mut stream,
                    409,
                    r#"{"status":"succeeded","reset_complete":true,"intent_id":"runtime-intent","provider_id":"microvm-exact","assignment_epoch":1}"#,
                );
            }
        }
    });
    let control = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", control.local_addr().unwrap());
    let control_server = std::thread::spawn(move || {
        for index in 0..5 {
            let (mut stream, _) = control.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let incoming = request(&mut stream);
            assert!(incoming.to_ascii_lowercase().contains("authorization:"));
            let body = if index == 0 {
                "<GetCallerIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><GetCallerIdentityResult><Account>116392764028</Account><Arn>arn:aws:iam::116392764028:user/fixture</Arn><UserId>fixture</UserId></GetCallerIdentityResult><ResponseMetadata><RequestId>fixture</RequestId></ResponseMetadata></GetCallerIdentityResponse>".to_owned()
            } else if index % 2 == 1 {
                serde_json::json!({"microvmId":"microvm-exact","state":"RUNNING","endpoint":runtime_endpoint,"imageArn":"image-fixture","imageVersion":"1","maximumDurationInSeconds":600,"startedAt":1}).to_string()
            } else {
                serde_json::json!({"authToken":{"X-aws-proxy-auth":"fixture-token"}}).to_string()
            };
            reply(&mut stream, 200, &body);
        }
    });
    let credentials = aws_credential_types::Credentials::new(
        "fixture-key",
        "fixture-secret",
        None,
        None,
        "fixture",
    );
    let config = aws_sdk_lambdamicrovms::Config::builder()
        .behavior_version_latest()
        .region(aws_sdk_lambdamicrovms::config::Region::new("us-west-2"))
        .credentials_provider(credentials.clone())
        .endpoint_url(&endpoint)
        .build();
    let identity = aws_sdk_sts::Config::builder()
        .behavior_version_latest()
        .region(aws_sdk_sts::config::Region::new("us-west-2"))
        .endpoint_url(&endpoint)
        .build();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut provider = runtime
        .block_on(Provider::verify_account(
            config,
            identity,
            aws_credential_types::provider::SharedCredentialsProvider::new(credentials),
            "116392764028",
            Duration::from_secs(3),
        ))
        .unwrap();
    provider.runtime_certificate = Some(reqwest::Certificate::from_der(certificate).unwrap());
    let mut maintenance = crate::service::daemon_services::MicrovmCleanupService::start(
        owner.clone(),
        provider.clone(),
        "116392764028".into(),
        "us-west-2".into(),
        Duration::from_millis(25),
        10,
        std::collections::BTreeMap::from([([1; 32], (Duration::from_secs(60), 100))]),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        maintenance.check().unwrap();
        let row = fixture
            .connect()
            .query_one(
                "SELECT state FROM microvm_workers WHERE intent_id='runtime-intent'",
                &[],
            )
            .unwrap();
        if row.get::<_, String>(0) == "idle" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "maintenance reset did not retain worker"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    maintenance.shutdown().unwrap();
    let next = "/nix/store/cccccccccccccccccccccccccccccccc-runtime.drv";
    crate::persistence::claim_shared_build(
        database,
        next,
        &[2; 32],
        "arm",
        kind,
        kind.capabilities(),
        None,
        &["/nix/store/dddddddddddddddddddddddddddddddd-output"],
    )
    .unwrap();
    crate::persistence::start_shared_build(database, next).unwrap();
    let attempt = crate::persistence::read_shared_build_attempt(database, next)
        .unwrap()
        .unwrap()
        .attempt_id;
    let selected = crate::persistence::select_microvm_worker(
        database,
        attempt,
        &[1; 32],
        Duration::from_secs(10),
        &[4; 32],
    )
    .unwrap()
    .unwrap();
    assert_eq!(selected.2, 2);
    provider
        .deliver_worker_assignment(
            &owner,
            &selected.0,
            &selected.1,
            &serde_json::json!({"intent_id":selected.0,"assignment_epoch":selected.2}).to_string(),
        )
        .unwrap();
    runtime_server.join().unwrap();
    control_server.join().unwrap();
    let row=fixture.connect().query_one("SELECT l.phase,l.reservation_released_at IS NULL,w.state FROM microvm_launches l JOIN microvm_workers w USING(intent_id) WHERE intent_id='runtime-intent'",&[]).unwrap();
    assert_eq!(row.get::<_, String>(0), "bound");
    assert!(row.get::<_, bool>(1));
    assert_eq!(row.get::<_, String>(2), "busy");
}
