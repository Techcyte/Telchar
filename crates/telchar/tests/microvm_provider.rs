//! Exercises the signed provider client against a real loopback HTTP boundary.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

#[derive(Debug)]
struct LaunchCredentials {
    database: String,
    expire_admission: bool,
    calls: std::sync::atomic::AtomicUsize,
}

impl aws_credential_types::provider::ProvideCredentials for LaunchCredentials {
    fn provide_credentials<'a>(
        &'a self,
    ) -> aws_credential_types::provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        aws_credential_types::provider::future::ProvideCredentials::new(async move {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
                let database = self.database.clone();
                let expire_admission = self.expire_admission;
                tokio::task::spawn_blocking(move || {
                    let sql = if expire_admission {
                        "UPDATE microvm_launches SET created_at = clock_timestamp() - interval '2 seconds', admission_cutoff = clock_timestamp() - interval '1 second' WHERE intent_id = 'intent-fixture'"
                    } else {
                        "UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second' WHERE owner_kind = 'daemon'"
                    };
                    postgres::Client::connect(&database, postgres::NoTls).unwrap()
                        .batch_execute(sql).unwrap();
                }).await.unwrap();
            }
            Ok(aws_credential_types::Credentials::new(
                "fixture-key",
                "fixture-secret",
                None,
                None,
                "test",
            ))
        })
    }
}

#[test]
fn ownership_loss_during_credentials_prevents_launch() {
    launch_exchange(true, "credentials-expired");
}

#[test]
fn admission_expiry_during_credentials_prevents_launch() {
    launch_exchange(true, "credentials-cutoff");
}

#[test]
fn reserved_launch_records_one_dispatch_and_quarantines_provider_failure() {
    launch_exchange(false, "active");
    launch_exchange(true, "active");
    launch_exchange(true, "expired");
    launch_exchange(true, "response-expired");
    launch_exchange(true, "payload-mismatch");
    launch_exchange(true, "region-mismatch");
}

#[test]
fn execution_closes_admission_without_releasing_provider_reservation() {
    launch_exchange(true, "execution-timeout");
    launch_exchange(false, "execution-failure");
    launch_exchange(false, "configured");
    launch_exchange(false, "configured-missing");
    launch_exchange(true, "configured-cancelled");
    launch_exchange(true, "configured-shutdown");
}

#[test]
fn launch_rejects_hook_coordinates_that_disagree_with_reservation() {
    for mismatch in [
        "hook-intent",
        "hook-lifetime",
        "hook-capability",
        "hook-schema",
    ] {
        launch_exchange(true, mismatch);
    }
}

#[test]
fn uncertain_http_outcomes_never_retry_or_release_capacity() {
    for fault in ["dropped", "malformed", "timeout"] {
        launch_exchange(false, fault);
    }
}

fn launch_exchange(success: bool, ownership: &'static str) {
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-provider.drv";
    let kind = if ownership.starts_with("configured") {
        telchar::backend::BackendKind::Microvm
    } else {
        telchar::backend::BackendKind::Local
    };
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[1; 32],
        "provider-fixture",
        kind,
        kind.capabilities(),
        None,
        &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-provider"],
    )
    .expect("fixture build claims");
    telchar::persistence::start_shared_build(fixture.url(), derivation).expect("attempt starts");
    let attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
        .expect("attempt reads")
        .expect("attempt exists");
    if matches!(
        ownership,
        "configured" | "configured-cancelled" | "configured-shutdown"
    ) {
        let source = r#"Derive([("out","/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-provider","","")],[],[],"aarch64-linux","/bin/sh",[],[("builder","/bin/sh"),("name","provider"),("out","/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-provider"),("system","aarch64-linux")])"#;
        let target = telchar::backend::BackendTarget::new(
            "provider-fixture",
            kind,
            "aarch64-linux",
            Vec::<String>::new(),
        )
        .unwrap();
        let request = telchar::build::BuildRequest::from_stored_derivation(
            derivation.as_bytes(),
            source.as_bytes(),
            &[target],
        )
        .unwrap();
        fixture.connect().execute("UPDATE shared_builds SET build_request = $1::text::jsonb, request_digest = $2 WHERE derivation_path = $3", &[&serde_json::to_string(&request).unwrap(), &&request.shared_build_digest()[..], &derivation]).unwrap();
    }
    let capability = telchar::microvm::Capability::generate().unwrap();
    let payload = capability
        .launch_payload(
            "wss://gateway.example/microvm",
            "127.0.0.1:443".parse().unwrap(),
            "intent-fixture",
            600,
        )
        .unwrap();
    let mut hook: serde_json::Value = serde_json::from_str(&payload).unwrap();
    match ownership {
        "hook-intent" => hook["intent_id"] = "foreign".into(),
        "hook-lifetime" => hook["lifetime_seconds"] = 601.into(),
        "hook-capability" => {
            hook["bearer"] = telchar::microvm::Capability::generate()
                .unwrap()
                .bearer()
                .into()
        }
        "hook-schema" => hook["extra"] = true.into(),
        _ => {}
    }
    let payload = hook.to_string();
    let intent = telchar::persistence::MicrovmLaunchIntent {
        intent_id: "intent-fixture".to_owned(),
        attempt_id: attempt.attempt_id,
        backend_name: "provider-fixture".to_owned(),
        account_id: "116392764028".to_owned(),
        region: "us-west-2".to_owned(),
        image_arn: "image-fixture".to_owned(),
        image_version: "1".to_owned(),
        configuration_digest: [1; 32],
        request_digest: telchar::microvm::launch_request_digest(
            "image-fixture",
            "1",
            "intent-fixture",
            &payload,
            600,
        ),
        client_token: "intent-fixture".to_owned(),
        credential_verifier: capability.verifier(),
        admission_cutoff: std::time::SystemTime::now() + Duration::from_secs(60),
        maximum_duration_seconds: 600,
    };
    if !ownership.starts_with("configured") {
        assert!(
            telchar::persistence::reserve_microvm_launch(fixture.url(), &intent, 1)
                .expect("capacity reserves")
        );
    }
    let owner = std::sync::Arc::new(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(20),
        )
        .expect("owner acquires"),
    );
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
                    if ownership.starts_with("credentials-") && headers.starts_with("POST / HTTP") {
                        let body = "<GetCallerIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><GetCallerIdentityResult><Account>116392764028</Account><Arn>arn:aws:iam::116392764028:user/fixture</Arn><UserId>fixture</UserId></GetCallerIdentityResult><ResponseMetadata><RequestId>fixture</RequestId></ResponseMetadata></GetCallerIdentityResponse>";
                        write!(socket, "HTTP/1.1 200 OK\r\ncontent-type: text/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
                        continue;
                    }
                    requests.push((headers, body));
                    if ownership == "response-expired" {
                        postgres::Client::connect(&control_url, postgres::NoTls).expect("fixture control connects").batch_execute("UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second' WHERE owner_kind = 'daemon'").expect("owner expires during request");
                    }
                    if ownership == "dropped" {
                        continue;
                    }
                    if ownership == "timeout" {
                        std::thread::sleep(Duration::from_millis(1300));
                        continue;
                    }
                    let (status, body) = if ownership == "malformed" {
                        ("200 OK", "{invalid".to_owned())
                    } else if success {
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
            .region(aws_sdk_lambdamicrovms::config::Region::new(
                if ownership == "region-mismatch" {
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
            .endpoint_url(endpoint.clone())
            .build();
        let provider = if ownership.starts_with("credentials-") {
            let credentials =
                aws_credential_types::provider::SharedCredentialsProvider::new(LaunchCredentials {
                    database: fixture.url().to_owned(),
                    expire_admission: ownership == "credentials-cutoff",
                    calls: std::sync::atomic::AtomicUsize::new(0),
                });
            let identity = aws_sdk_sts::Config::builder()
                .behavior_version_latest()
                .region(aws_sdk_sts::config::Region::new("us-west-2"))
                .credentials_provider(credentials.clone())
                .endpoint_url(endpoint)
                .build();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(telchar::microvm::Provider::verify_account(
                    config,
                    identity,
                    credentials,
                    "116392764028",
                    Duration::from_secs(1),
                ))
                .unwrap()
        } else {
            telchar::microvm::Provider::new(config, Duration::from_secs(1)).expect("provider")
        };
        let payload = if ownership == "payload-mismatch" {
            "{\"extra\":true}"
        } else {
            &payload
        };
        let started = std::time::Instant::now();
        let result = if ownership.starts_with("execution-") || ownership.starts_with("configured") {
            let sessions = telchar::microvm::rendezvous::ExecutionSessions::new(1).unwrap();
            let executor = telchar::microvm::execution::Execution {
                provider: &provider,
                owner: &owner,
                sessions: &sessions,
                store: telchar::store::daemon::GatewayStoreEndpoint::parse(
                    "unix:///not-used/microvm.sock",
                )
                .unwrap(),
                staging: std::env::temp_dir(),
                limits: telchar::microvm::session::Limits {
                    maximum_paths: 10,
                    maximum_nar_bytes: 1024,
                    maximum_total_bytes: 2048,
                    metadata_bytes: 8192,
                    chunk_bytes: 1024,
                },
            };
            if ownership.starts_with("configured") {
                let deployment = r#"
name = "provider-fixture"
system = "aarch64-linux"
account_id = "116392764028"
region = "us-west-2"
image_arn = "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture"
image_version = "1"
maximum_concurrent_builds = 1
maximum_duration_seconds = 600
execution_timeout_seconds = 540
admission_timeout_seconds = 30
provider_timeout_seconds = 1
cleanup_interval_seconds = 5
callback_url = "wss://gateway.example/microvm"
callback_address = "127.0.0.1:443"
staging_directory = "/tmp"
maximum_paths = 10
maximum_nar_bytes = 1024
maximum_total_bytes = 2048
metadata_bytes = 8192
chunk_bytes = 1024
"#;
                let policy =
                    telchar::service::config::MicrovmBackendConfig::parse(deployment).unwrap();
                if matches!(
                    ownership,
                    "configured" | "configured-cancelled" | "configured-shutdown"
                ) {
                    let request = telchar::persistence::microvm_attempt_request(
                        owner.database(),
                        attempt.attempt_id,
                        policy.name(),
                    )
                    .unwrap()
                    .unwrap();
                    let mut build = telchar::backend::BuildExecution::new(
                        "scheduler-request",
                        &request,
                        Duration::from_secs(2),
                    )
                    .unwrap();
                    build.set_target_name("wrong-target").unwrap();
                    assert!(executor.run_build(&policy, &build, |_| {}).is_err());
                    let count: i64 = fixture
                        .connect()
                        .query_one("SELECT count(*) FROM microvm_launches", &[])
                        .unwrap()
                        .get(0);
                    assert_eq!(count, 0);
                    build.set_target_name(policy.name()).unwrap();
                    if ownership == "configured-shutdown" {
                        use telchar::backend::BuildBackend;
                        let service = std::sync::Arc::new(
                            telchar::microvm::service::Service::start(
                                policy.clone(),
                                provider.clone(),
                                owner.clone(),
                                TcpListener::bind("127.0.0.1:0").unwrap(),
                                executor.store.clone(),
                            )
                            .unwrap(),
                        );
                        let configuration_file = tempfile::NamedTempFile::new().unwrap();
                        std::fs::write(configuration_file.path(), format!(
                            "[backends]\nmicrovm_bind = \"127.0.0.1:7444\"\n[[backends.microvm]]\n{deployment}"
                        )).unwrap();
                        let configuration =
                            telchar::service::config::ServiceConfig::load_from_default(
                                configuration_file.path(),
                            )
                            .unwrap();
                        let backends = telchar::backend::routing::ConfiguredBackends::new(
                            &configuration,
                            Some(executor.store.clone()),
                        )
                        .unwrap()
                        .attach_microvm_services(
                            &configuration,
                            vec![std::sync::Arc::clone(&service)],
                        )
                        .unwrap();
                        let mut backend = backends
                            .executor(
                                owner.database().clone(),
                                std::sync::Arc::new(
                                    telchar::shared_build::SharedBuildRegistry::new(),
                                ),
                            )
                            .unwrap();
                        assert_eq!(
                            backend.selected_target("aarch64-linux", &[]).unwrap(),
                            *policy.target()
                        );
                        let mut control = fixture.connect();
                        let started = std::time::Instant::now();
                        let error = backend.execute_with_logs(&build, &mut |_| Ok(()), &mut || {
                            let bound: bool = control.query_one("SELECT EXISTS(SELECT 1 FROM microvm_launches WHERE phase = 'bound')", &[]).unwrap().get(0);
                            if bound {
                                service.shutdown().unwrap();
                            }
                            Ok(false)
                        }).unwrap_err();
                        assert!(started.elapsed() < Duration::from_secs(1));
                        assert_eq!(error.to_string(), "MicroVM service stopped");
                        let row = control.query_one("SELECT phase, reservation_released_at IS NULL FROM microvm_launches", &[]).unwrap();
                        assert_eq!(row.get::<_, String>(0), "closed");
                        assert!(row.get::<_, bool>(1));
                        assert!(
                            backend.execute(&build).is_err(),
                            "stopped handles cannot dispatch"
                        );
                        assert_eq!(server.join().unwrap().len(), 1);
                        return;
                    }
                    if ownership == "configured-cancelled" {
                        let mut control = fixture.connect();
                        let started = std::time::Instant::now();
                        let error = executor.execute_build(&policy, &build, &mut |_| Ok(()), &mut || {
                            Ok(control.query_one("SELECT EXISTS(SELECT 1 FROM microvm_launches WHERE phase = 'bound')", &[]).unwrap().get(0))
                        }).unwrap_err();
                        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
                        assert!(started.elapsed() < Duration::from_secs(1));
                        let row = control.query_one("SELECT phase, reservation_released_at IS NULL FROM microvm_launches", &[]).unwrap();
                        assert_eq!(row.get::<_, String>(0), "closed");
                        assert!(row.get::<_, bool>(1));
                        assert_eq!(server.join().unwrap().len(), 1);
                        return;
                    }
                    assert!(executor.run_build(&policy, &build, |_| {}).is_err());
                } else {
                    assert!(
                        executor
                            .run_configured(&policy, attempt.attempt_id, |_| {})
                            .is_err()
                    );
                }
                if ownership == "configured-missing" {
                    let count: i64 = fixture
                        .connect()
                        .query_one("SELECT count(*) FROM microvm_launches", &[])
                        .unwrap()
                        .get(0);
                    assert_eq!(count, 0, "missing admitted request must not reserve");
                    assert!(
                        server.join().unwrap().is_empty(),
                        "missing request must not dispatch"
                    );
                    return;
                }
                let row = fixture.connect().query_one("SELECT intent_id, phase, configuration_digest, reservation_released_at IS NULL FROM microvm_launches", &[]).unwrap();
                assert_ne!(row.get::<_, String>(0), "intent-fixture");
                assert_eq!(row.get::<_, String>(1), "ambiguous");
                assert_eq!(row.get::<_, Vec<u8>>(2), policy.fingerprint());
                assert!(row.get::<_, bool>(3));
                assert!(
                    executor
                        .run_configured(&policy, attempt.attempt_id, |_| {})
                        .is_err()
                );
                let requests = server.join().unwrap();
                assert_eq!(requests.len(), 1, "capacity prevents another dispatch");
                return;
            }
            assert!(
                executor
                    .run_reserved(
                        &intent,
                        payload,
                        started + Duration::from_millis(500),
                        |_| {}
                    )
                    .is_err()
            );
            Ok(None)
        } else {
            provider.launch_reserved(&owner, &intent, payload)
        };
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "bounded launch: {ownership}"
        );
        if ownership.starts_with("execution-") || ownership == "credentials-cutoff" {
            assert_eq!(result.unwrap(), None);
        } else if ownership != "active" {
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
        let repeat = provider.launch_reserved(&owner, &intent, payload);
        if matches!(
            ownership,
            "active"
                | "credentials-cutoff"
                | "dropped"
                | "malformed"
                | "timeout"
                | "execution-timeout"
                | "execution-failure"
        ) {
            assert_eq!(repeat.expect("repeat has no dispatch grant"), None);
        } else {
            assert!(repeat.is_err());
        }
    }
    let requests = server.join().expect("server joins");
    let rejected_before_dispatch = matches!(
        ownership,
        "expired"
            | "credentials-cutoff"
            | "credentials-expired"
            | "payload-mismatch"
            | "region-mismatch"
            | "hook-intent"
            | "hook-lifetime"
            | "hook-capability"
            | "hook-schema"
    );
    assert_eq!(requests.len(), if rejected_before_dispatch { 0 } else { 1 });
    if !rejected_before_dispatch {
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
            "expired"
            | "credentials-cutoff"
            | "credentials-expired"
            | "payload-mismatch"
            | "region-mismatch"
            | "hook-intent"
            | "hook-lifetime"
            | "hook-capability"
            | "hook-schema" => "prepared",
            "response-expired" => "dispatching",
            "execution-timeout" => "closed",
            _ if success => "bound",
            _ => "ambiguous",
        }
    );
    assert_eq!(
        row.get::<_, Option<String>>(1).as_deref(),
        if success && matches!(ownership, "active" | "execution-timeout") {
            Some("microvm-exact")
        } else {
            None
        }
    );
    assert!(row.get::<_, bool>(2));
}
