use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use telchar::microvm::session::Limits;
use telchar::nomad::protocol::BuildOutcome;
use telchar::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

mod microvm_daemon;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/microvm/canonical-store.sh"
);

fn provider_response(
    binding: Option<std::sync::mpsc::SyncSender<serde_json::Value>>,
) -> (telchar::microvm::Provider, std::thread::JoinHandle<()>) {
    let attest = binding.is_some();
    let (endpoint, response) = provider_server(binding);
    provider_client(endpoint, response, attest)
}

fn provider_server(
    binding: Option<std::sync::mpsc::SyncSender<serde_json::Value>>,
) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let attest = binding.is_some();
    let response = std::thread::spawn(move || {
        for identity in if attest {
            vec![true, false]
        } else {
            vec![false]
        } {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("provider accept: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                header.push(byte[0]);
                assert!(header.len() < 32768);
            }
            let header = String::from_utf8(header).unwrap();
            let length: usize = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            assert!(length < 32768);
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            assert!(
                header
                    .to_ascii_lowercase()
                    .contains("authorization: aws4-hmac-sha256 credential=fixture-key/")
            );
            if identity {
                assert!(header.starts_with("POST / "));
                assert!(
                    String::from_utf8(body)
                        .unwrap()
                        .contains("Action=GetCallerIdentity")
                );
                let body = "<GetCallerIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><GetCallerIdentityResult><Account>116392764028</Account><Arn>arn:aws:iam::116392764028:user/fixture</Arn><UserId>fixture</UserId></GetCallerIdentityResult><ResponseMetadata><RequestId>fixture</RequestId></ResponseMetadata></GetCallerIdentityResponse>";
                write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: text/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
                continue;
            }
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if let Some(binding) = &binding {
                let payload: serde_json::Value =
                    serde_json::from_str(request["runHookPayload"].as_str().unwrap()).unwrap();
                binding.send(payload).unwrap();
            } else {
                assert_eq!(request["clientToken"], "build-token");
            }
            let body = serde_json::json!({"microvmId":"microvm-build", "state":"PENDING", "endpoint":"https://unused.invalid", "imageArn":"fixture", "imageVersion":"1", "maximumDurationInSeconds":60, "startedAt":1}).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (endpoint, response)
}

fn provider_client(
    endpoint: String,
    response: std::thread::JoinHandle<()>,
    attest: bool,
) -> (telchar::microvm::Provider, std::thread::JoinHandle<()>) {
    let credentials =
        aws_credential_types::Credentials::new("fixture-key", "fixture-secret", None, None, "test");
    let config = aws_sdk_lambdamicrovms::Config::builder()
        .behavior_version_latest()
        .region(aws_sdk_lambdamicrovms::config::Region::new("us-west-2"))
        .credentials_provider(credentials.clone())
        .endpoint_url(&endpoint)
        .build();
    let provider = if attest {
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
                aws_credential_types::provider::SharedCredentialsProvider::new(credentials),
                "116392764028",
                Duration::from_secs(2),
            ))
            .unwrap()
    } else {
        telchar::microvm::Provider::new(config, Duration::from_secs(2)).unwrap()
    };
    (provider, response)
}

#[test]
fn dedicated_worker_builds_and_returns_registered_output() {
    if std::env::var_os("MICROVM_TEST_STORE_URI").is_none() {
        for case in [
            "single",
            "multiple",
            "cancelled",
            "fenced",
            "configured",
            "configured-multiple",
            "configured-failure",
            "configured-cancelled",
            "configured-fenced",
            "configured-corrupt-output",
            "configured-fixed-output",
            "configured-daemon",
            "configured-daemon-multiple",
            "configured-daemon-fixed-output",
        ] {
            run_isolated_build(case);
        }
        return;
    }
    run_build();
}

fn run_isolated_build(case: &str) {
    let database = telchar_test_support::postgres::PostgresFixture::start();
    let output = Command::new("bash")
        .env("MICROVM_TEST_DATABASE", database.url())
        .env("MICROVM_TEST_CASE", case)
        .arg(FIXTURE)
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "dedicated_worker_builds_and_returns_registered_output",
            "--test-threads=1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_build() {
    let case = std::env::var("MICROVM_TEST_CASE").unwrap();
    let multiple = matches!(
        case.as_str(),
        "multiple" | "configured-multiple" | "configured-daemon-multiple"
    );
    let configured = case.starts_with("configured");
    let build_failure = case == "configured-failure";
    let corrupt_output = case == "configured-corrupt-output";
    let fixed_output = matches!(
        case.as_str(),
        "configured-fixed-output" | "configured-daemon-fixed-output"
    );
    let system = if configured {
        "aarch64-linux"
    } else {
        "x86_64-linux"
    };
    let root = std::path::PathBuf::from(std::env::var_os("MICROVM_STORE_ROOT").unwrap());
    let endpoint =
        GatewayStoreEndpoint::parse(&std::env::var("MICROVM_TEST_STORE_URI").unwrap()).unwrap();
    let source = root.join("input");
    std::fs::write(&source, b"dedicated-worker-success\n").unwrap();
    let added = Command::new("nix-store")
        .args(["--store", "local", "--add"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert!(added.stderr.is_empty());
    let input = String::from_utf8(added.stdout).unwrap().trim().to_owned();
    let mut store = GatewayStoreConnection::connect(&endpoint).unwrap();
    assert!(store.query_path_info(input.as_bytes()).unwrap().is_some());
    let hash = {
        use sha2::{Digest, Sha256};
        Sha256::digest(b"dedicated-worker-success\n")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let fixed_path;
    let output = if fixed_output {
        let result = Command::new("nix-store")
            .args(["--print-fixed-path", "sha256", &hash, "dedicated"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stderr.is_empty());
        fixed_path = String::from_utf8(result.stdout).unwrap();
        fixed_path.trim()
    } else {
        "/nix/store/ffffffffffffffffffffffffffffffff-dedicated-output"
    };
    let secondary = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-dedicated-reference";
    let outputs = if multiple {
        vec![output, secondary]
    } else {
        vec![output]
    };
    let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-dedicated.drv";
    assert!(!store.is_valid_path(output.as_bytes()).unwrap());
    let extra_script = if multiple {
        "; printf '%s' \"$out\" > \"$reference\""
    } else {
        ""
    };
    let extra_output = if multiple {
        format!(r#",("reference","{secondary}","","")"#)
    } else {
        String::new()
    };
    let extra_environment = if multiple {
        format!(r#",("reference","{secondary}")"#)
    } else {
        String::new()
    };
    let script = if build_failure {
        "exit 42".to_owned()
    } else {
        format!("read -r value < {input}; printf '%s\\n' \"$value\" > \"$out\"{extra_script}")
    };
    let script = serde_json::to_string(&script).unwrap();
    let (hash_algorithm, output_hash) = if fixed_output {
        ("sha256", hash.as_str())
    } else {
        ("", "")
    };
    let derivation = format!(
        r#"Derive([("out","{output}","{hash_algorithm}","{output_hash}"){extra_output}],[],["{input}"],"{system}","/bin/sh",["-c",{script}],[("out","{output}"){extra_environment},("system","{system}"),("builder","/bin/sh"),("name","dedicated")])"#
    );
    let request = telchar::build::BuildRequest::from_stored_derivation(
        drv.as_bytes(),
        derivation.as_bytes(),
        &[telchar::backend::BackendTarget::new(
            "fixture",
            telchar::backend::BackendKind::Local,
            system,
            [] as [&str; 0],
        )
        .unwrap()],
    )
    .unwrap();
    let database = std::env::var("MICROVM_TEST_DATABASE").unwrap();
    telchar::persistence::migrate(&database).unwrap();
    let local = if configured {
        telchar::backend::BackendKind::Microvm
    } else {
        telchar::backend::BackendKind::Local
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let gateway = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let gateway_address = gateway.local_addr().unwrap();
    let capability = telchar::microvm::Capability::generate().unwrap();
    let ownership_lost = matches!(case.as_str(), "fenced" | "configured-fenced");
    let (binding_sender, binding_receiver) = std::sync::mpsc::sync_channel(1);
    let gateway_thread = if case.starts_with("configured-daemon") {
        drop(gateway);
        let root = root.clone();
        let database = database.clone();
        let endpoint = endpoint.clone();
        std::thread::spawn(move || {
            microvm_daemon::run(
                &root,
                &database,
                &endpoint,
                address,
                gateway_address,
                &request,
                binding_sender,
            )
        })
    } else {
        telchar::persistence::claim_shared_build_with_request(
            &database,
            drv,
            &request.shared_build_digest(),
            "build",
            local,
            local.capabilities(),
            None,
            &outputs,
            &request,
        )
        .unwrap();
        telchar::persistence::start_shared_build(&database, drv).unwrap();
        let attempt = telchar::persistence::read_shared_build_attempt(&database, drv)
            .unwrap()
            .unwrap();
        let payload = capability
            .launch_payload(
                &format!("wss://localhost:{}/microvm", address.port()),
                address,
                "build-intent",
                60,
            )
            .unwrap();
        let intent = telchar::persistence::MicrovmLaunchIntent {
            intent_id: "build-intent".into(),
            attempt_id: attempt.attempt_id,
            backend_name: "build".into(),
            account_id: "116392764028".into(),
            region: "us-west-2".into(),
            image_arn: "fixture".into(),
            image_version: "1".into(),
            configuration_digest: [1; 32],
            request_digest: telchar::microvm::launch_request_digest(
                "fixture",
                "1",
                "build-token",
                &payload,
                60,
            ),
            client_token: "build-token".into(),
            credential_verifier: capability.verifier(),
            admission_cutoff: std::time::SystemTime::now() + Duration::from_secs(60),
            maximum_duration_seconds: 60,
        };
        if !configured {
            assert!(telchar::persistence::reserve_microvm_launch(&database, &intent, 1).unwrap());
        }

        let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
            &database,
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(
            telchar::persistence::microvm_session_request(owner.database(), "build-intent")
                .unwrap()
                .is_none()
        );
        let owner = std::sync::Arc::new(owner);
        let sessions =
            std::sync::Arc::new(telchar::microvm::rendezvous::ExecutionSessions::new(1).unwrap());
        let mut gateway = Some(gateway);
        let mut callback = if configured {
            None
        } else {
            Some(
                telchar::microvm::callback_service::CallbackService::start(
                    gateway.take().unwrap(),
                    owner.clone(),
                    sessions.clone(),
                    1,
                    1,
                    Duration::from_secs(5),
                )
                .unwrap(),
            )
        };
        let gateway_store = endpoint.clone();
        let staging = root.clone();
        let execution_intent = intent.clone();
        std::thread::spawn(move || {
            let (provider, response) = provider_response(configured.then_some(binding_sender));
            let limits = Limits {
                maximum_paths: 10,
                maximum_nar_bytes: 1024 * 1024,
                maximum_total_bytes: 2 * 1024 * 1024,
                metadata_bytes: 8192,
                chunk_bytes: 256 * 1024,
            };
            let execution = telchar::microvm::execution::Execution {
                provider: &provider,
                owner: &owner,
                sessions: &sessions,
                store: gateway_store,
                staging,
                limits,
            };
            let result = if configured {
                let deployment = format!(
                    r#"
name = "build"
system = "aarch64-linux"
account_id = "116392764028"
region = "us-west-2"
image_arn = "arn:aws:lambda:us-west-2:116392764028:microvm-image:builder"
image_version = "1"
maximum_concurrent_builds = 1
maximum_duration_seconds = 60
execution_timeout_seconds = 50
admission_timeout_seconds = 20
provider_timeout_seconds = 2
cleanup_interval_seconds = 5
callback_url = "wss://localhost:{}/microvm"
callback_address = "{}"
staging_directory = "{}"
maximum_paths = 10
maximum_nar_bytes = 1048576
maximum_total_bytes = 2097152
metadata_bytes = 8192
chunk_bytes = 262144
"#,
                    address.port(),
                    address,
                    execution.staging.display()
                );
                let policy =
                    telchar::service::config::MicrovmBackendConfig::parse(&deployment).unwrap();
                let service = Arc::new(
                    telchar::microvm::service::Service::start(
                        policy,
                        provider.clone(),
                        owner.clone(),
                        gateway.take().unwrap(),
                        execution.store.clone(),
                    )
                    .unwrap(),
                );
                let configuration_file = execution.staging.join("service.toml");
                std::fs::write(&configuration_file, format!(
                "[backends]\nmicrovm_bind = \"{gateway_address}\"\n[[backends.microvm]]\n{deployment}"
            )).unwrap();
                let configuration =
                    telchar::service::config::ServiceConfig::load_from_default(&configuration_file)
                        .unwrap();
                let backends = telchar::backend::routing::ConfiguredBackends::new(
                    &configuration,
                    Some(execution.store.clone()),
                )
                .unwrap()
                .attach_microvm_services(&configuration, vec![service.clone()])
                .unwrap();
                let mut backend = backends
                    .executor(
                        owner.database().clone(),
                        Arc::new(telchar::shared_build::SharedBuildRegistry::new()),
                    )
                    .unwrap();
                use telchar::backend::BuildBackend;
                let target = backend.selected_target(request.system(), &[]).unwrap();
                assert_eq!(target.kind(), telchar::backend::BackendKind::Microvm);
                let mut build = telchar::backend::BuildExecution::new(
                    "configured-worker",
                    &request,
                    Duration::from_secs(15),
                )
                .unwrap();
                build.set_target_name(target.name()).unwrap();
                let result = backend.execute_with_logs(&build, &mut |_| Ok(()), &mut || Ok(false));
                let shutdown = service.shutdown();
                if ownership_lost {
                    if let Err(error) = shutdown {
                        assert_eq!(error.to_string(), "MicroVM listener failed");
                    }
                    assert!(backend.selected_target(request.system(), &[]).is_err());
                } else {
                    shutdown.unwrap();
                }
                result.map(|result| {
                    assert_eq!(result.status(), telchar::backend::BuildStatus::Built);
                    assert_eq!(result.outputs(), request.expected_outputs());
                    BuildOutcome::Built
                })
            } else {
                execution
                    .run_reserved(
                        &execution_intent,
                        &payload,
                        std::time::Instant::now() + Duration::from_secs(15),
                        |_| {},
                    )
                    .map(|result| result.outcome)
            };
            response.join().unwrap();
            let shutdown = callback
                .as_mut()
                .map_or(Ok(()), |callback| callback.shutdown());
            if ownership_lost {
                if let Err(error) = shutdown {
                    assert_eq!(error.to_string(), "MicroVM listener failed");
                }
            } else {
                shutdown.unwrap();
            }
            result
        })
    };
    let binding = root.join("binding.json");
    let trust = root.join("trust.der");
    std::fs::write(&trust, include_bytes!("fixtures/microvm-cert.der")).unwrap();
    let mut worker_binding = if configured {
        binding_receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
    } else {
        serde_json::json!({
            "endpoint": format!("wss://localhost:{}/microvm", address.port()), "address": address.to_string(),
            "intent_id": "build-intent", "bearer": capability.bearer(), "lifetime_seconds": 15
        })
    };
    worker_binding.as_object_mut().unwrap().remove("version");
    worker_binding["provider_id"] = "microvm-build".into();
    let intent_id = worker_binding["intent_id"].as_str().unwrap().to_owned();
    use sha2::Digest;
    let verifier: [u8; 32] =
        sha2::Sha256::digest(worker_binding["bearer"].as_str().unwrap().as_bytes()).into();
    std::fs::write(&binding, serde_json::to_vec(&worker_binding).unwrap()).unwrap();
    std::fs::set_permissions(&binding, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut worker = Command::new("bash")
        .arg(FIXTURE)
        .arg("/bin/sh")
        .args([
            "-c",
            "exec \"$1\" \"$2\" \"$3\" \"$MICROVM_TEST_STORE_URI\"",
            "worker",
        ])
        .arg(env!("CARGO_BIN_EXE_telchar-microvm-worker"))
        .arg(&binding)
        .arg(&trust)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    listener.set_nonblocking(true).unwrap();
    let started = std::time::Instant::now();
    let stream = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "worker did not connect"
        );
        if let Some(status) = worker.try_wait().unwrap() {
            panic!(
                "worker exited {status}: {:?}",
                worker.wait_with_output().unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let config = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![include_bytes!("fixtures/microvm-cert.der").to_vec().into()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(
            include_bytes!("fixtures/microvm-key.der").to_vec().into(),
        ),
    )
    .unwrap();
    let tls = rustls::StreamOwned::new(
        rustls::ServerConnection::new(Arc::new(config)).unwrap(),
        stream,
    );
    #[allow(clippy::result_large_err)]
    let upgrade = |_: &tungstenite::handshake::server::Request,
                   mut response: tungstenite::handshake::server::Response| {
        response.headers_mut().insert(
            "sec-websocket-protocol",
            "telchar-microvm-v1".parse().unwrap(),
        );
        Ok(response)
    };
    let mut socket = tungstenite::accept_hdr(tls, upgrade).unwrap();
    use telchar::nomad::protocol::{
        FrameKind, NarMetadata, PathSet, ProtocolLimits, decode_metadata, read_frame,
    };
    use tungstenite::client::IntoClientRequest;
    let mut request = format!("ws://{gateway_address}/microvm")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "sec-websocket-protocol",
        "telchar-microvm-v1".parse().unwrap(),
    );
    let connection = std::net::TcpStream::connect(gateway_address).unwrap();
    connection
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    connection
        .set_write_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let (mut upstream, _) = tungstenite::client(request, connection).unwrap();
    upstream.send(socket.read().unwrap()).unwrap();
    socket.send(upstream.read().unwrap()).unwrap();
    if matches!(
        case.as_str(),
        "cancelled" | "fenced" | "configured-cancelled" | "configured-fenced"
    ) {
        let started = std::time::Instant::now();
        if !ownership_lost {
            assert!(telchar::persistence::cancel_microvm_launch(&database, &intent_id).unwrap());
        } else {
            let mut database = postgres::Client::connect(&database, postgres::NoTls).unwrap();
            assert_eq!(database.execute("UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second' WHERE owner_kind = 'daemon'", &[]).unwrap(), 1);
        }
        let result = gateway_thread.join().unwrap();
        drop(upstream);
        drop(socket);
        let worker = worker.wait_with_output().unwrap();
        assert!(!worker.status.success());
        let diagnostic = String::from_utf8(worker.stderr).unwrap();
        assert!(diagnostic.contains("MicroVM worker failed"), "{diagnostic}");
        assert!(
            diagnostic.contains("Canonical store fixture retained:"),
            "{diagnostic}"
        );
        assert!(result.is_err());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{case} idle session did not stop promptly"
        );
        assert!(!store.is_valid_path(output.as_bytes()).unwrap());
        if configured {
            let mut control = postgres::Client::connect(&database, postgres::NoTls).unwrap();
            let row = control.query_one(
                "SELECT phase, session_consumed_at IS NOT NULL, reservation_released_at IS NULL FROM microvm_launches WHERE intent_id = $1", &[&intent_id],
            ).unwrap();
            assert_eq!(
                row.get::<_, String>(0),
                if ownership_lost { "bound" } else { "closed" }
            );
            assert!(row.get::<_, bool>(1));
            assert!(row.get::<_, bool>(2));
        }
        return;
    }
    loop {
        let message = socket.read().unwrap();
        let frame = read_frame(
            &mut message.clone().into_data().as_ref(),
            ProtocolLimits::new(8192, 256 * 1024),
        )
        .unwrap();
        if corrupt_output && frame.kind() == FrameKind::OutputNar {
            let mut bytes = message.into_data().to_vec();
            let content = b"dedicated-worker-success";
            let offset = bytes
                .windows(content.len())
                .position(|window| window == content)
                .expect("output NAR contains fixture content");
            bytes[offset] ^= 1;
            upstream
                .send(tungstenite::Message::Binary(bytes.into()))
                .unwrap();
            break;
        }
        upstream.send(message).unwrap();
        match frame.kind() {
            FrameKind::InputRequest => {
                let requested: PathSet = decode_metadata(frame.metadata(), 8192).unwrap();
                for _ in requested.paths {
                    loop {
                        let message = upstream.read().unwrap();
                        let frame = read_frame(
                            &mut message.clone().into_data().as_ref(),
                            ProtocolLimits::new(8192, 256 * 1024),
                        )
                        .unwrap();
                        let chunk: NarMetadata = decode_metadata(frame.metadata(), 8192).unwrap();
                        socket.send(message).unwrap();
                        if chunk.final_chunk {
                            break;
                        }
                    }
                }
            }
            FrameKind::OutputNar => {
                let chunk: NarMetadata = decode_metadata(frame.metadata(), 8192).unwrap();
                if chunk.final_chunk {
                    socket.send(upstream.read().unwrap()).unwrap();
                }
            }
            FrameKind::BuildResult => break,
            _ => {}
        }
    }
    let result = gateway_thread.join().unwrap();
    if corrupt_output {
        drop(upstream);
        drop(socket);
    }
    assert!(
        !telchar::persistence::admit_microvm_session(
            &database,
            &intent_id,
            "microvm-build",
            &verifier
        )
        .unwrap()
    );
    let worker = worker.wait_with_output().unwrap();
    if build_failure || corrupt_output {
        assert!(!worker.status.success());
        assert!(worker.stdout.is_empty());
        let diagnostic = String::from_utf8(worker.stderr).unwrap();
        assert!(
            diagnostic.starts_with("MicroVM worker failed\nCanonical store fixture retained: "),
            "{diagnostic}"
        );
        assert_eq!(diagnostic.lines().count(), 2, "{diagnostic}");
        let error = result.unwrap_err();
        if build_failure {
            assert_eq!(error.to_string(), "MicroVM build failed");
        }
        assert!(!store.is_valid_path(output.as_bytes()).unwrap());
        let mut control = postgres::Client::connect(&database, postgres::NoTls).unwrap();
        let row = control.query_one("SELECT phase, reservation_released_at IS NULL FROM microvm_launches WHERE intent_id = $1", &[&intent_id]).unwrap();
        assert_eq!(row.get::<_, String>(0), "closed");
        assert!(row.get::<_, bool>(1));
        return;
    }
    assert!(
        worker.status.success(),
        "{result:?}: {}",
        String::from_utf8_lossy(&worker.stderr)
    );
    assert!(worker.stdout.is_empty());
    assert!(
        worker.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&worker.stderr)
    );
    assert_eq!(result.unwrap(), BuildOutcome::Built);
    let mut control = postgres::Client::connect(&database, postgres::NoTls).unwrap();
    let row = control.query_one(
        "SELECT phase, provider_id, session_consumed_at IS NOT NULL, reservation_released_at IS NULL FROM microvm_launches WHERE intent_id = $1",
        &[&intent_id],
    ).unwrap();
    assert_eq!(row.get::<_, String>(0), "closed");
    assert_eq!(row.get::<_, String>(1), "microvm-build");
    assert!(row.get::<_, bool>(2));
    assert!(
        row.get::<_, bool>(3),
        "build success cannot release provider capacity"
    );
    for expected in outputs {
        assert!(store.is_valid_path(expected.as_bytes()).unwrap());
    }
    if fixed_output {
        let info = store.query_path_info(output.as_bytes()).unwrap().unwrap();
        assert!(
            info.content_address()
                .unwrap()
                .starts_with(b"fixed:sha256:")
        );
        assert!(info.references().is_empty());
    }
    if multiple {
        assert_eq!(std::fs::read(secondary).unwrap(), output.as_bytes());
        let info = store
            .query_path_info(secondary.as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(info.references(), &[output.as_bytes().to_vec()]);
    }
    assert_eq!(
        std::fs::read(output).unwrap(),
        b"dedicated-worker-success\n"
    );
}
