//! MicroVM bootstrap admission over real loopback sockets and PostgreSQL.

mod support;

use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;

#[test]
fn socket_admission_requires_exact_single_use_capability() {
    let fixture = support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migrate");
    let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-rendezvous.drv";
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[1; 32],
        "rendezvous",
        telchar::backend::BackendKind::Local,
        telchar::backend::BackendKind::Local.capabilities(),
        None,
        &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-rendezvous"],
    )
    .expect("claim");
    telchar::persistence::start_shared_build(fixture.url(), derivation).expect("start");
    let attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
        .expect("read")
        .expect("attempt");
    let capability = telchar::microvm::Capability::generate().expect("capability");
    let intent = telchar::persistence::MicrovmLaunchIntent {
        intent_id: "rendezvous-intent".into(),
        attempt_id: attempt.attempt_id,
        backend_name: "rendezvous".into(),
        account_id: "116392764028".into(),
        region: "us-west-2".into(),
        image_arn: "image-fixture".into(),
        image_version: "1".into(),
        configuration_digest: [1; 32],
        request_digest: [2; 32],
        client_token: "rendezvous-token".into(),
        credential_verifier: capability.verifier(),
        admission_cutoff: std::time::SystemTime::now() + Duration::from_secs(60),
        maximum_duration_seconds: 600,
    };
    assert!(
        telchar::persistence::reserve_microvm_launch(fixture.url(), &intent, 1).expect("reserve")
    );
    assert!(
        telchar::persistence::dispatch_microvm_launch(fixture.url(), &intent.intent_id)
            .expect("dispatch")
    );
    assert!(
        telchar::persistence::bind_microvm_launch(
            fixture.url(),
            &intent.intent_id,
            "microvm-exact"
        )
        .expect("bind")
    );
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(30),
    )
    .expect("owner");
    for (bearer, id, accepted) in [
        ("wrong", "microvm-exact", false),
        (capability.bearer(), "microvm-other", false),
        (capability.bearer(), "microvm-exact", true),
        (capability.bearer(), "microvm-exact", false),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                let (stream, _) = listener.accept().expect("accept");
                telchar::microvm::rendezvous::accept_session(
                    stream,
                    &owner,
                    Instant::now() + Duration::from_secs(2),
                )
                .is_ok()
            });
            let stream = TcpStream::connect(address).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("bounded client");
            let mut request = format!("ws://{address}/microvm")
                .into_client_request()
                .expect("request");
            request.headers_mut().insert(
                "sec-websocket-protocol",
                "telchar-microvm-v1".parse().expect("protocol"),
            );
            let (mut socket, response) = tungstenite::client(request, stream).expect("upgrade");
            assert_eq!(
                response.headers()["sec-websocket-protocol"],
                "telchar-microvm-v1"
            );
            socket.send(tungstenite::Message::Binary(serde_json::to_vec(&serde_json::json!({"intent_id": intent.intent_id, "provider_id": id, "bearer": bearer})).expect("bootstrap").into())).expect("send");
            assert_eq!(server.join().expect("server"), accepted);
        });
    }
}
