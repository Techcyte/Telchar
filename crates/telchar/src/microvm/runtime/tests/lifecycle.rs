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

fn accept(listener: &std::net::TcpListener) -> std::net::TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "fixture accept deadline"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
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

#[allow(dead_code)]
#[path = "../../../../../../tests/telemetry/collector.rs"]
mod collector;

#[test]
fn maintenance_retains_worker_and_dispatch_admits_next_assignment() {
    // Isolate the global exporter/subscriber and its environment from other tests.
    if std::env::var_os("TELCHAR_RESET_TRACE_TEST").is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .env("TELCHAR_RESET_TRACE_TEST", "1")
            .args(["--exact", "microvm::runtime::tests::lifecycle::maintenance_retains_worker_and_dispatch_admits_next_assignment", "--nocapture", "--test-threads=1"])
            .status().unwrap();
        assert!(status.success());
        return;
    }
    let collector = collector::start_collector();
    unsafe {
        std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint());
        std::env::set_var("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc");
    }
    let telemetry = telchar_telemetry::Telemetry::initialize("telchar", "test").unwrap();
    let parent = tracing::info_span!(parent: None, "backend.execution");
    let entered = parent.enter();
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    crate::persistence::migrate(fixture.url()).unwrap();
    let owner = std::sync::Arc::new(
        crate::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(60),
        )
        .unwrap(),
    );
    let callback = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let callback_address = callback.local_addr().unwrap();
    let raw = include_str!("../../../../tests/fixtures/microvm.toml")
        .replace("internal-builder.example", "localhost")
        .replace(
            "wss://gateway.example/microvm",
            &format!("wss://localhost:{}/microvm", callback_address.port()),
        );
    let config = crate::service::config::MicrovmBackendConfig::parse(&raw).unwrap();
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
    crate::persistence::record_microvm_attempt_trace_context(
        database,
        attempt,
        &telchar_telemetry::TraceContext::capture_current(),
    )
    .unwrap();
    let intent = crate::persistence::MicrovmLaunchIntent {
        intent_id: "runtime-intent".into(),
        attempt_id: attempt,
        backend_name: "arm".into(),
        account_id: "116392764028".into(),
        region: "us-west-2".into(),
        image_arn: "image-fixture".into(),
        image_version: "1".into(),
        vpc_egress_connector_arn: "connector".into(),
        configuration_digest: config.fingerprint(),
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
    let runtime_endpoint = format!("localhost:{}", tls.local_addr().unwrap().port());
    let certificate =
        include_bytes!("../../../../../telchar-nomad-worker/tests/fixtures/microvm-cert.der");
    let key = include_bytes!("../../../../../telchar-nomad-worker/tests/fixtures/microvm-key.der");
    let sessions =
        std::sync::Arc::new(crate::microvm::rendezvous::ExecutionSessions::new(1).unwrap());
    let callback_sessions = sessions.clone();
    let callback_owner = owner.clone();
    let admitted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let callback_admitted = admitted.clone();
    let callback_server = std::thread::spawn(move || {
        let stream = accept(&callback);
        let pending = crate::microvm::rendezvous::PendingConnections::new(1, 1).unwrap();
        callback_sessions
            .accept(
                stream,
                &callback_owner,
                std::time::Instant::now() + Duration::from_secs(10),
                pending.reserve().unwrap(),
            )
            .unwrap();
        callback_admitted.store(true, std::sync::atomic::Ordering::Release);
    });
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
            let stream = accept(&tls);
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
                let mut validation = std::process::Command::new("python3")
                    .env("PYTHONDONTWRITEBYTECODE", "1")
                    .arg("-c")
                    .arg(
                        r#"import json,pathlib,sys,tempfile
from unittest.mock import patch
sys.path.insert(0,sys.argv[1])
from execution import ExecutionState
hook=json.load(sys.stdin)
value=json.loads(hook['runHookPayload'])
with tempfile.TemporaryDirectory() as directory:
    root=pathlib.Path(directory)
    trust=root/'trust'
    trust.write_text('fixture')
    state=ExecutionState([],[],[],root/'binding',trust)
    prior=dict(value)
    prior['assignment_epoch']=1
    prior['bearer']='A'*43
    with patch('execution.supervise',return_value=True):
        assert state.start_hook({'microvmId':hook['microvmId'],'runHookPayload':json.dumps(prior)})
        state.join()
        assert state.start_hook(hook)
        state.join()
    assert state.report()['assignment_epoch']==2
    assert state.report()['reset_complete']
"#,
                    )
                    .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/microvm"))
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                validation
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(serde_json::to_string(&payload).unwrap().as_bytes())
                    .unwrap();
                let checked = validation.wait_with_output().unwrap();
                assert!(
                    checked.status.success(),
                    "{}",
                    String::from_utf8_lossy(&checked.stderr)
                );
                assert!(checked.stdout.is_empty());
                assert!(checked.stderr.is_empty());
                assert_eq!(payload["microvmId"], "microvm-exact");
                let payload: serde_json::Value =
                    serde_json::from_str(payload["runHookPayload"].as_str().unwrap()).unwrap();
                assert_eq!(payload["assignment_epoch"], 2);
                assert_eq!(payload["intent_id"], "runtime-intent");
                reply(&mut stream, 200, "{}");
                use tungstenite::client::IntoClientRequest;
                let connection = std::net::TcpStream::connect(callback_address).unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut handshake = format!("ws://{callback_address}/microvm")
                    .into_client_request()
                    .unwrap();
                handshake.headers_mut().insert(
                    "sec-websocket-protocol",
                    "telchar-microvm-v1".parse().unwrap(),
                );
                let (mut socket, _) = tungstenite::client(handshake, connection).unwrap();
                socket.send(tungstenite::Message::Binary(serde_json::to_vec(&serde_json::json!({"intent_id":payload["intent_id"],"assignment_epoch":payload["assignment_epoch"],"provider_id":"microvm-exact","bearer":payload["bearer"]})).unwrap().into())).unwrap();
                while !admitted.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                socket.close(None).unwrap();
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
            let mut stream = accept(&control);
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
    let provider_config = aws_sdk_lambdamicrovms::Config::builder()
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
            provider_config,
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
        std::collections::BTreeMap::from([(config.fingerprint(), (Duration::from_secs(60), 100))]),
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
    let source = r#"Derive([("out","/nix/store/dddddddddddddddddddddddddddddddd-output","","")],[],[],"aarch64-linux","/bin/sh",[],[("builder","/bin/sh"),("name","runtime"),("out","/nix/store/dddddddddddddddddddddddddddddddd-output"),("system","aarch64-linux")])"#;
    let target =
        crate::backend::BackendTarget::new("arm", kind, "aarch64-linux", Vec::<String>::new())
            .unwrap();
    let build = crate::build::BuildRequest::from_stored_derivation(
        next.as_bytes(),
        source.as_bytes(),
        &[target],
    )
    .unwrap();
    fixture.connect().execute("UPDATE shared_builds SET build_request=$1::text::jsonb,request_digest=$2 WHERE derivation_path=$3", &[&serde_json::to_string(&build).unwrap(),&&build.shared_build_digest()[..],&next]).unwrap();
    crate::persistence::start_shared_build(database, next).unwrap();
    let attempt = crate::persistence::read_shared_build_attempt(database, next)
        .unwrap()
        .unwrap()
        .attempt_id;
    let executor = crate::microvm::execution::Execution {
        provider: &provider,
        owner: &owner,
        sessions: &sessions,
        store: crate::store::daemon::GatewayStoreEndpoint::parse("unix:///not-used/microvm.sock")
            .unwrap(),
        staging: std::env::temp_dir(),
        limits: config.transfer_limits(),
    };
    let error = executor
        .run_configured(&config, attempt, |_| {})
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        error.to_string(),
        "Nomad transfer manifest path count is invalid"
    );
    callback_server.join().unwrap();
    let assignment=fixture.connect().query_one("SELECT assignment_epoch,admitted_at IS NOT NULL FROM microvm_assignments WHERE intent_id='runtime-intent' ORDER BY assignment_epoch DESC LIMIT 1",&[]).unwrap();
    assert_eq!(assignment.get::<_, i64>(0), 2);
    assert!(assignment.get::<_, bool>(1));
    runtime_server.join().unwrap();
    control_server.join().unwrap();
    let row=fixture.connect().query_one("SELECT l.phase,l.reservation_released_at IS NULL,w.state FROM microvm_launches l JOIN microvm_workers w USING(intent_id) WHERE intent_id='runtime-intent'",&[]).unwrap();
    assert_eq!(row.get::<_, String>(0), "closed");
    assert!(row.get::<_, bool>(1));
    assert_eq!(row.get::<_, String>(2), "draining");
    drop(entered);
    drop(parent);
    telemetry.shutdown();
    let parents = collector.spans("telchar", "backend.execution");
    let confirmations = collector.spans("telchar", "microvm.gateway.reset_confirmation");
    assert_eq!(parents.len(), 1);
    assert_eq!(confirmations.len(), 1);
    collector::Collector::assert_child(&parents[0], &confirmations[0]);
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    assert!(confirmations[0].attributes.iter().any(|attribute| {
        attribute.key == "confirmed"
            && attribute
                .value
                .as_ref()
                .and_then(|value| value.value.as_ref())
                == Some(&Value::BoolValue(true))
    }));
    collector.assert_no_private_markers(&["fixture-secret", "fixture-token"]);
    if let Some(path) = std::env::var_os("TELCHAR_RESET_TRACE_ARTIFACT") {
        collector.write_trace_artifact(path);
    }
}
