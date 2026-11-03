//! Component test: real PostgreSQL and production transfer observers, simulated gateway metadata.
//! Valid failed terminal delivery after recovery is covered; no successful NAR/output promotion or real Nix execution is claimed.

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use telchar::microvm::rendezvous::{ExecutionSessions, PendingConnections};
use telchar::nomad::protocol::{FrameKind, ProtocolLimits, read_frame};

fn send_metadata(
    client: &mut tungstenite::WebSocket<TcpStream>,
    kind: FrameKind,
    metadata: serde_json::Value,
) {
    let mut bytes = Vec::new();
    telchar::nomad::protocol::write_frame(
        &mut bytes,
        &telchar::nomad::protocol::Frame::new(kind, serde_json::to_vec(&metadata).unwrap(), vec![]),
        ProtocolLimits::new(8192, 256),
    )
    .unwrap();
    client
        .send(tungstenite::Message::Binary(bytes.into()))
        .unwrap();
}
use telchar_test_support::postgres::PostgresFixture;
use tungstenite::client::IntoClientRequest;

// Same worker-wire contract as gateway_daemon_connection_contract's path-info fixture.
fn serve_input_metadata(listener: std::os::unix::net::UnixListener, input: &[u8]) {
    use nix_worker_protocol::{
        CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_LAST,
    };
    use std::io::{Read, Write};
    fn integer(stream: &mut impl Write, value: u64) {
        stream.write_all(&value.to_le_bytes()).unwrap();
    }
    fn read_integer(stream: &mut impl Read) -> u64 {
        let mut bytes = [0; 8];
        stream.read_exact(&mut bytes).unwrap();
        u64::from_le_bytes(bytes)
    }
    fn string(stream: &mut impl Write, value: &[u8]) {
        integer(stream, value.len() as u64);
        stream.write_all(value).unwrap();
        stream
            .write_all(&vec![0; (8 - value.len() % 8) % 8])
            .unwrap();
    }
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    assert_eq!(read_integer(&mut stream), CLIENT_WORKER_MAGIC);
    assert_eq!(read_integer(&mut stream), LATEST_WORKER_VERSION.to_wire());
    integer(&mut stream, SERVER_WORKER_MAGIC);
    integer(&mut stream, LATEST_WORKER_VERSION.to_wire());
    stream.flush().unwrap();
    assert_eq!(read_integer(&mut stream), 0);
    integer(&mut stream, 0);
    stream.flush().unwrap();
    assert_eq!(read_integer(&mut stream), 0);
    assert_eq!(read_integer(&mut stream), 0);
    string(&mut stream, b"2.34.8");
    integer(&mut stream, 1);
    integer(&mut stream, STDERR_LAST);
    stream.flush().unwrap();
    assert_eq!(read_integer(&mut stream), 26);
    let length = read_integer(&mut stream) as usize;
    assert_eq!(length, input.len());
    let mut path = vec![0; length.next_multiple_of(8)];
    stream.read_exact(&mut path).unwrap();
    assert_eq!(&path[..length], input);
    integer(&mut stream, STDERR_LAST);
    integer(&mut stream, 1);
    string(&mut stream, b"");
    string(
        &mut stream,
        b"6c2be2f12a168605ebcba3782286c38eaf3f5d787b7a8bb24a540d2267ff68e1",
    );
    integer(&mut stream, 0);
    integer(&mut stream, 42);
    integer(&mut stream, 136);
    integer(&mut stream, 0);
    integer(&mut stream, 0);
    string(&mut stream, b"");
    stream.flush().unwrap();
}

#[test]
fn admitted_transfer_is_retained_during_recovery_and_interrupted_on_expiry_or_takeover() {
    let input_path = "/nix/store/cccccccccccccccccccccccccccccccc-ownership-input";
    for interruption in ["recover", "expire", "takeover"] {
        let mut fixture = PostgresFixture::start();
        telchar::persistence::migrate(fixture.url()).unwrap();
        let duration = if interruption == "recover" {
            Duration::from_secs(10)
        } else {
            Duration::from_secs(3)
        };
        let owner = Arc::new(
            telchar::service::singleton_ownership::SingletonOwnership::acquire(
                fixture.url(),
                duration,
            )
            .unwrap(),
        );
        let renewal = owner.with_renewal(Duration::from_millis(100)).unwrap();
        let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-ownership.drv";
        let output = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-ownership";
        let derivation = format!(
            r#"Derive([("out","{output}","","")],[],["{input_path}"],"aarch64-linux","/bin/sh",[],[("out","{output}"),("system","aarch64-linux"),("builder","/bin/sh"),("name","ownership")])"#
        );
        let request = telchar::build::BuildRequest::from_stored_derivation(
            drv.as_bytes(),
            derivation.as_bytes(),
            &[telchar::backend::BackendTarget::new(
                "arm",
                telchar::backend::BackendKind::Microvm,
                "aarch64-linux",
                [] as [&str; 0],
            )
            .unwrap()],
        )
        .unwrap();
        let kind = telchar::backend::BackendKind::Microvm;
        telchar::persistence::claim_shared_build_with_request(
            owner.database(),
            drv,
            &request.shared_build_digest(),
            "arm",
            kind,
            kind.capabilities(),
            None,
            &[output],
            &request,
        )
        .unwrap();
        telchar::persistence::start_shared_build(owner.database(), drv).unwrap();
        let attempt = telchar::persistence::read_shared_build_attempt(owner.database(), drv)
            .unwrap()
            .unwrap();
        let capability = telchar::microvm::Capability::generate().unwrap();
        let intent = telchar::persistence::MicrovmLaunchIntent {
            intent_id: "ownership-transfer".into(),
            attempt_id: attempt.attempt_id,
            backend_name: "arm".into(),
            account_id: "000000000001".into(),
            region: "us-west-2".into(),
            image_arn: "fixture".into(),
            image_version: "1".into(),
            vpc_egress_connector_arn:
                "arn:aws:lambda:us-west-2:000000000001:network-connector:fixture".into(),
            configuration_digest: [1; 32],
            request_digest: [2; 32],
            client_token: "ownership-transfer".into(),
            credential_verifier: capability.verifier(),
            admission_cutoff: SystemTime::now() + Duration::from_secs(30),
            maximum_duration_seconds: 60,
        };
        assert!(
            telchar::persistence::reserve_microvm_launch(owner.database(), &intent, 1).unwrap()
        );
        assert!(
            telchar::persistence::dispatch_microvm_launch(owner.database(), &intent.intent_id)
                .unwrap()
        );
        assert!(
            telchar::persistence::bind_microvm_launch(
                owner.database(),
                &intent.intent_id,
                "microvm-fixture"
            )
            .unwrap()
        );
        let sessions = ExecutionSessions::new(1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let waiting = sessions.register(&intent.intent_id, deadline).unwrap();
        let pending = PendingConnections::new(1, 1).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::scope(|scope| {
            let accept = scope.spawn(|| {
                sessions
                    .accept(
                        listener.accept().unwrap().0,
                        &owner,
                        deadline,
                        pending.reserve().unwrap(),
                    )
                    .unwrap();
            });
            let mut upgrade = format!("ws://{address}/microvm")
                .into_client_request()
                .unwrap();
            upgrade.headers_mut().insert(
                "sec-websocket-protocol",
                "telchar-microvm-v1".parse().unwrap(),
            );
            let stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let (mut client, _) = tungstenite::client(upgrade, stream).unwrap();
            client.send(tungstenite::Message::Binary(serde_json::to_vec(&serde_json::json!({
                "intent_id": intent.intent_id, "provider_id": "microvm-fixture", "bearer": capability.bearer(),
            })).unwrap().into())).unwrap();
            accept.join().unwrap();
            let connection = waiting.receive_active(&owner).unwrap();
            let staging = tempfile::tempdir().unwrap();
            let transfer_owner = Arc::clone(&owner);
            let gateway = tempfile::tempdir().unwrap();
            let path = gateway.path().join("gateway.sock");
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            let metadata =
                scope.spawn(move || serve_input_metadata(listener, input_path.as_bytes()));
            let endpoint = telchar::store::daemon::GatewayStoreEndpoint::parse(&format!(
                "unix://{}",
                path.display()
            ))
            .unwrap();
            let transfer = scope.spawn(move || {
                telchar::microvm::session::run_admitted(
                    connection,
                    &transfer_owner,
                    endpoint,
                    staging.path().into(),
                    telchar::microvm::session::Limits {
                        maximum_paths: 10,
                        maximum_nar_bytes: 1024,
                        maximum_total_bytes: 2048,
                        metadata_bytes: 8192,
                        chunk_bytes: 256,
                    },
                    |_| {},
                )
            });
            let manifest = match client.read() {
                Ok(message) => message.into_data(),
                Err(error) => panic!(
                    "transfer ended before manifest: {error}; server: {:?}",
                    transfer.join().unwrap()
                ),
            };
            assert_eq!(
                read_frame(&mut manifest.as_ref(), ProtocolLimits::new(8192, 256))
                    .unwrap()
                    .kind(),
                FrameKind::InputManifest
            );
            metadata.join().unwrap();
            fixture.stop();
            let until = Instant::now() + Duration::from_secs(2);
            while matches!(owner.admission_available(), Ok(true)) {
                assert!(Instant::now() < until);
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                !transfer.is_finished(),
                "transport outage interrupted admitted transfer"
            );
            if interruption == "recover" {
                fixture.restart();
                let until = Instant::now() + Duration::from_secs(3);
                while !owner.admission_available().unwrap() {
                    assert!(Instant::now() < until);
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert!(!transfer.is_finished(), "transfer did not survive recovery");
                // Valid application progress and terminal delivery after recovery (no output success claim).
                send_metadata(
                    &mut client,
                    FrameKind::ValidPaths,
                    serde_json::json!({"paths": [input_path]}),
                );
                send_metadata(
                    &mut client,
                    FrameKind::InputRequest,
                    serde_json::json!({"paths": []}),
                );
                send_metadata(
                    &mut client,
                    FrameKind::BuildStarted,
                    serde_json::json!({"derivation_path": drv}),
                );
                let result = telchar::nomad::protocol::BuildResultMetadata {
                    outcome: telchar::nomad::protocol::BuildOutcome::Failed,
                    diagnostic: Some("fixture completed after ownership recovery".into()),
                };
                send_metadata(
                    &mut client,
                    FrameKind::BuildResult,
                    serde_json::to_value(&result).unwrap(),
                );
                assert_eq!(transfer.join().unwrap().unwrap(), result);
                return;
            } else if interruption == "takeover" {
                fixture.restart();
                fixture.expire_singleton_ownership("daemon");
                let replacement =
                    telchar::service::singleton_ownership::SingletonOwnership::acquire(
                        fixture.url(),
                        duration,
                    )
                    .unwrap();
                assert!(replacement.generation() > owner.generation());
                let until = Instant::now() + Duration::from_secs(3);
                while !transfer.is_finished() {
                    assert!(Instant::now() < until);
                    std::thread::sleep(Duration::from_millis(10));
                }
                replacement.verify().unwrap();
            } else {
                let until = Instant::now() + Duration::from_secs(4);
                while !transfer.is_finished() {
                    assert!(Instant::now() < until);
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            assert!(transfer.join().unwrap().is_err());
        });
        drop(renewal);
    }
}
