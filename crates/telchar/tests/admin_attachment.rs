//! Root socket attachment uses attested provider metadata and fenced persistence.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn root_attachment_checks_correspondence_and_post_lookup_ownership() {
    if let Ok(url) = std::env::var("TELCHAR_ADMIN_FIXTURE_DATABASE") {
        exercise(&url);
        return;
    }
    assert_ne!(unsafe { libc::geteuid() }, 0);
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    let output = Command::new("unshare")
        .args(["--user", "--map-root-user"])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "root_attachment_checks_correspondence_and_post_lookup_ownership",
            "--nocapture",
        ])
        .env("TELCHAR_ADMIN_FIXTURE_DATABASE", fixture.url())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

fn exercise(url: &str) {
    let database = telchar::persistence::Database::connect(url).unwrap();
    telchar::persistence::migrate(&database).unwrap();
    let owner = Arc::new(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            url,
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let inspection = database.clone();
    let server = std::thread::spawn(move || {
        for case in ["identity", "mismatch", "released", "fenced"] {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "provider request missing");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 32768);
            }
            let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            assert!(headers.contains("credential=fixture-key/"));
            let (body, content_type) = if case == "identity" {
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(length < 4096);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                assert!(
                    String::from_utf8(body)
                        .unwrap()
                        .contains("Action=GetCallerIdentity")
                );
                ("<GetCallerIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><GetCallerIdentityResult><Account>116392764028</Account><Arn>arn:aws:iam::116392764028:user/fixture</Arn><UserId>fixture</UserId></GetCallerIdentityResult><ResponseMetadata><RequestId>fixture</RequestId></ResponseMetadata></GetCallerIdentityResponse>".to_owned(), "text/xml")
            } else {
                assert!(headers.starts_with("get /2025-09-09/microvms/microvm-exact "));
                if case == "fenced" {
                    inspection.connection().unwrap().batch_execute("UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second' WHERE owner_kind = 'daemon'").unwrap();
                }
                (serde_json::json!({"microvmId":"microvm-exact", "state":"RUNNING", "endpoint":"https://unused.invalid", "imageArn":"arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture", "imageVersion": if case == "mismatch" { "2" } else { "1" }, "maximumDurationInSeconds":600, "startedAt":1}).to_string(), "application/json")
            };
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let credentials =
        aws_credential_types::Credentials::new("fixture-key", "fixture-secret", None, None, "test");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let provider = runtime
        .block_on(telchar::microvm::Provider::verify_account(
            aws_sdk_lambdamicrovms::Config::builder()
                .behavior_version_latest()
                .region(aws_sdk_lambdamicrovms::config::Region::new("us-west-2"))
                .credentials_provider(credentials.clone())
                .endpoint_url(&endpoint)
                .build(),
            aws_sdk_sts::Config::builder()
                .behavior_version_latest()
                .region(aws_sdk_sts::config::Region::new("us-west-2"))
                .credentials_provider(credentials.clone())
                .endpoint_url(&endpoint)
                .build(),
            aws_credential_types::provider::SharedCredentialsProvider::new(credentials),
            "116392764028",
            Duration::from_secs(2),
        ))
        .unwrap();
    drop(runtime);
    let config = telchar::service::config::MicrovmBackendConfig::parse(
        r#"
name = "arm"
system = "aarch64-linux"
account_id = "116392764028"
region = "us-west-2"
image_arn = "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture"
image_version = "1"
maximum_concurrent_builds = 4
maximum_duration_seconds = 60
execution_timeout_seconds = 50
admission_timeout_seconds = 3
provider_timeout_seconds = 2
cleanup_interval_seconds = 300
callback_url = "wss://gateway.example/microvm"
callback_address = "192.0.2.1:443"
staging_directory = "/unused"
maximum_paths = 10
maximum_nar_bytes = 1024
maximum_total_bytes = 2048
metadata_bytes = 1024
chunk_bytes = 1024
"#,
    )
    .unwrap();
    let service = Arc::new(
        telchar::microvm::service::Service::start(
            config,
            provider,
            Arc::clone(&owner),
            TcpListener::bind("127.0.0.1:0").unwrap(),
            telchar::store::daemon::GatewayStoreEndpoint::parse("unix:///unused").unwrap(),
        )
        .unwrap(),
    );
    for case in ["mismatch", "released", "fenced"] {
        let derivation = format!("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-{case}.drv");
        let kind = telchar::backend::BackendKind::Microvm;
        telchar::persistence::claim_shared_build(
            owner.database(),
            &derivation,
            &[1; 32],
            "arm",
            kind,
            kind.capabilities(),
            None,
            &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-admin"],
        )
        .unwrap();
        telchar::persistence::start_shared_build(owner.database(), &derivation).unwrap();
        let attempt =
            telchar::persistence::read_shared_build_attempt(owner.database(), &derivation)
                .unwrap()
                .unwrap();
        let intent = telchar::persistence::MicrovmLaunchIntent {
            intent_id: case.into(),
            attempt_id: attempt.attempt_id,
            backend_name: "arm".into(),
            account_id: "116392764028".into(),
            region: "us-west-2".into(),
            image_arn: "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture".into(),
            image_version: "1".into(),
            configuration_digest: [1; 32],
            request_digest: [2; 32],
            client_token: case.into(),
            credential_verifier: [3; 32],
            admission_cutoff: std::time::SystemTime::now() + Duration::from_secs(60),
            maximum_duration_seconds: 600,
        };
        assert!(
            telchar::persistence::reserve_microvm_launch(owner.database(), &intent, 4).unwrap()
        );
        assert!(telchar::persistence::dispatch_microvm_launch(owner.database(), case).unwrap());
        assert!(telchar::persistence::cancel_microvm_launch(owner.database(), case).unwrap());
        if case == "released" {
            assert!(
                telchar::persistence::release_microvm_capacity_risk(
                    owner.database(),
                    &telchar::persistence::MicrovmRiskRelease {
                        intent_id: case,
                        provider_id: None,
                        actor_uid: 0,
                        reason: "Risk accepted",
                        evidence_reference: "incident:risk",
                        accept_remote_execution_risk: true
                    }
                )
                .unwrap()
            );
        }
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("admin.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let serving = Arc::clone(&service);
        let fenced = owner.database().clone();
        let handler = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            telchar::service::admin::serve(stream, &fenced, &[serving]).unwrap();
        });
        let output = Command::new(env!("CARGO_BIN_EXE_telchar"))
            .env_clear()
            .args(["admin", "microvm", "attach-identity", "--socket"])
            .arg(&socket)
            .args([
                "--intent",
                case,
                "--provider-id",
                "microvm-exact",
                "--reason",
                "Recovered exact launch response",
                "--evidence",
                "incident:launch-response",
            ])
            .output()
            .unwrap();
        handler.join().unwrap();
        assert_eq!(output.status.success(), case == "released", "{output:?}");
        if case == "released" {
            assert!(output.stderr.is_empty(), "{output:?}");
            assert_eq!(
                output.stdout,
                b"{\"released\":false,\"attached\":true,\"rejected\":false}\n"
            );
            assert!(
                telchar::persistence::pending_microvm_cleanup_identity(
                    &database,
                    case,
                    "microvm-exact"
                )
                .unwrap()
            );
            assert!(
                !telchar::persistence::admit_microvm_session(
                    owner.database(),
                    case,
                    "microvm-exact",
                    &[3; 32]
                )
                .unwrap()
            );
        } else {
            assert!(output.stdout.is_empty());
            assert_eq!(output.stderr, b"telchar: administrative request rejected\n");
        }
        let row = database.connection().unwrap().query_one("SELECT provider_id, phase, reservation_released_at IS NOT NULL, (SELECT count(*) FROM microvm_resolutions WHERE intent_id = $1 AND action = 'attach-identity') FROM microvm_launches WHERE intent_id = $1", &[&case]).unwrap();
        assert_eq!(
            row.get::<_, Option<String>>(0).as_deref(),
            if case == "released" {
                Some("microvm-exact")
            } else {
                None
            }
        );
        assert_eq!(
            row.get::<_, String>(1),
            if case == "released" {
                "closed"
            } else {
                "ambiguous"
            }
        );
        assert_eq!(row.get::<_, bool>(2), case == "released");
        assert_eq!(row.get::<_, i64>(3), i64::from(case == "released"));
    }
    server.join().unwrap();
    service.shutdown().unwrap();
}
