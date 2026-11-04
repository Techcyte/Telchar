//! Shared-listener handoff preserves exact execution identity.

use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant, SystemTime};
use telchar::microvm::rendezvous::{ExecutionSessions, PendingConnections};
use tungstenite::client::IntoClientRequest;

#[test]
fn shared_listener_delivers_reordered_connections_only_to_live_waiters() {
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(30),
    )
    .unwrap();
    let database = owner.database().clone();
    let mut capabilities = Vec::new();
    for (index, hash) in ["a", "b", "c"].iter().enumerate() {
        let derivation = format!("/nix/store/{}-handoff.drv", hash.repeat(32));
        telchar::persistence::claim_shared_build(
            &database,
            &derivation,
            &[index as u8; 32],
            "handoff",
            telchar::backend::BackendKind::Local,
            telchar::backend::BackendKind::Local.capabilities(),
            None,
            &["/nix/store/dddddddddddddddddddddddddddddddd-handoff"],
        )
        .unwrap();
        telchar::persistence::start_shared_build(&database, &derivation).unwrap();
        let attempt = telchar::persistence::read_shared_build_attempt(&database, &derivation)
            .unwrap()
            .unwrap();
        let capability = telchar::microvm::Capability::generate().unwrap();
        let intent = telchar::persistence::MicrovmLaunchIntent {
            intent_id: format!("intent-{index}"),
            attempt_id: attempt.attempt_id,
            backend_name: "handoff".into(),
            account_id: "116392764028".into(),
            region: "us-west-2".into(),
            image_arn: "fixture".into(),
            image_version: "1".into(),
            vpc_egress_connector_arn:
                "arn:aws:lambda:us-west-2:116392764028:network-connector:builder".into(),
            configuration_digest: [1; 32],
            request_digest: [2; 32],
            client_token: format!("token-{index}"),
            credential_verifier: capability.verifier(),
            admission_cutoff: SystemTime::now() + Duration::from_secs(60),
            maximum_duration_seconds: 600,
        };
        assert!(telchar::persistence::reserve_microvm_launch(&database, &intent, 3).unwrap());
        assert!(
            telchar::persistence::dispatch_microvm_launch(&database, &intent.intent_id).unwrap()
        );
        assert!(
            telchar::persistence::bind_microvm_launch(
                &database,
                &intent.intent_id,
                &format!("microvm-{index}")
            )
            .unwrap()
        );
        capabilities.push(capability);
    }
    let sessions = ExecutionSessions::new(3).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let first = sessions.register("intent-0", deadline).unwrap();
    assert!(sessions.register("intent-0", deadline).is_err());
    let second = sessions.register("intent-1", deadline).unwrap();
    let abandoned = sessions.register("intent-2", deadline).unwrap();
    assert!(sessions.register("intent-3", deadline).is_err());
    drop(abandoned);
    let pending = PendingConnections::new(2, 2).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    for (index, accepted) in [(1, true), (0, true), (1, false), (2, false)] {
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                let (stream, _) = listener.accept().unwrap();
                sessions
                    .accept(stream, &owner, deadline, pending.reserve().unwrap())
                    .is_ok()
            });
            let stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = format!("ws://{address}/microvm")
                .into_client_request()
                .unwrap();
            request.headers_mut().insert(
                "sec-websocket-protocol",
                "telchar-microvm-v1".parse().unwrap(),
            );
            let (mut socket, _) = tungstenite::client(request, stream).unwrap();
            socket.send(tungstenite::Message::Binary(serde_json::to_vec(&serde_json::json!({
                "assignment_epoch": 1, "intent_id": format!("intent-{index}"), "provider_id": format!("microvm-{index}"),
                "bearer": capabilities[index].bearer(),
            })).unwrap().into())).unwrap();
            assert_eq!(server.join().unwrap(), accepted);
            if accepted {
                let connection = if index == 0 {
                    first.receive_active(&owner)
                } else {
                    second.receive_active(&owner)
                }
                .unwrap();
                assert_eq!(connection.intent_id(), format!("intent-{index}"));
                let (_, mut admitted) = connection.into_parts();
                admitted
                    .send(tungstenite::Message::Binary(vec![index as u8].into()))
                    .unwrap();
                assert_eq!(socket.read().unwrap().into_data().as_ref(), &[index as u8]);
            }
        });
    }
    drop(first);
    drop(second);
    let initial = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-handoff.drv";
    let attempt = telchar::persistence::read_shared_build_attempt(&database, initial)
        .unwrap()
        .unwrap()
        .attempt_id;
    telchar::persistence::collect_shared_build(&database, initial).unwrap();
    telchar::persistence::complete_shared_build_success(
        &database,
        initial,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(telchar::persistence::reset_microvm_worker(&database, "intent-0", 1, attempt).unwrap());
    let next = "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-handoff.drv";
    telchar::persistence::claim_shared_build(
        &database,
        next,
        &[4; 32],
        "handoff",
        telchar::backend::BackendKind::Local,
        telchar::backend::BackendKind::Local.capabilities(),
        None,
        &["/nix/store/ffffffffffffffffffffffffffffffff-handoff"],
    )
    .unwrap();
    telchar::persistence::start_shared_build(&database, next).unwrap();
    let attempt = telchar::persistence::read_shared_build_attempt(&database, next)
        .unwrap()
        .unwrap()
        .attempt_id;
    assert!(
        telchar::persistence::claim_microvm_worker(
            &database,
            "intent-0",
            1,
            attempt,
            Duration::from_secs(10)
        )
        .unwrap()
    );
    let capability = telchar::microvm::Capability::generate().unwrap();
    assert!(
        telchar::persistence::authorize_microvm_assignment(
            &database,
            "intent-0",
            2,
            &capability.verifier()
        )
        .unwrap()
    );
    let waiting = sessions.register("intent-0", deadline).unwrap();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let (stream, _) = listener.accept().unwrap();
            sessions
                .accept(stream, &owner, deadline, pending.reserve().unwrap())
                .unwrap();
        });
        let mut request = format!("ws://{address}/microvm")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            "telchar-microvm-v1".parse().unwrap(),
        );
        let (mut socket, _) =
            tungstenite::client(request, TcpStream::connect(address).unwrap()).unwrap();
        socket.send(tungstenite::Message::Binary(serde_json::to_vec(&serde_json::json!({
            "assignment_epoch":2, "intent_id":"intent-0", "provider_id":"microvm-0", "bearer":capability.bearer()
        })).unwrap().into())).unwrap();
        server.join().unwrap();
        let connection = waiting.receive_active(&owner).unwrap();
        assert_eq!(connection.assignment_epoch(), 2);
    });
    drop(waiting);
    assert!(!fixture.connect().query_one("SELECT session_consumed_at IS NOT NULL FROM microvm_launches WHERE intent_id = 'intent-2'", &[]).unwrap().get::<_, bool>(0));
    let cancelled = sessions
        .register("intent-2", Instant::now() + Duration::from_secs(3))
        .unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            assert!(
                telchar::persistence::cancel_microvm_launch(owner.database(), "intent-2").unwrap()
            );
        });
        let started = Instant::now();
        assert!(cancelled.receive_active(&owner).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancelled wait stops promptly"
        );
    });
    let expired = sessions.register("expired", Instant::now());
    assert!(expired.is_err());
    let timeout = sessions
        .register("timeout", Instant::now() + Duration::from_millis(30))
        .unwrap();
    assert!(timeout.receive().is_err());
}
