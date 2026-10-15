use telchar_test_support::postgres::PostgresFixture;

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;

#[test]
fn wss_worker_reaches_exact_fenced_gateway_admission_once() {
    let database = PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("migrate");
    let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-tls.drv";
    telchar::persistence::claim_shared_build(
        database.url(),
        derivation,
        &[1; 32],
        "tls",
        telchar::backend::BackendKind::Local,
        telchar::backend::BackendKind::Local.capabilities(),
        None,
        &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-tls"],
    )
    .expect("claim");
    telchar::persistence::start_shared_build(database.url(), derivation).expect("start");
    let attempt = telchar::persistence::read_shared_build_attempt(database.url(), derivation)
        .unwrap()
        .unwrap();
    let capability = telchar::microvm::Capability::generate().unwrap();
    let intent = telchar::persistence::MicrovmLaunchIntent {
        intent_id: "tls-intent".into(),
        attempt_id: attempt.attempt_id,
        backend_name: "tls".into(),
        account_id: "116392764028".into(),
        region: "us-west-2".into(),
        image_arn: "fixture".into(),
        image_version: "1".into(),
        configuration_digest: [1; 32],
        request_digest: [2; 32],
        client_token: "tls-token".into(),
        credential_verifier: capability.verifier(),
        admission_cutoff: std::time::SystemTime::now() + Duration::from_secs(60),
        maximum_duration_seconds: 600,
    };
    assert!(telchar::persistence::reserve_microvm_launch(database.url(), &intent, 1).unwrap());
    assert!(
        telchar::persistence::dispatch_microvm_launch(database.url(), &intent.intent_id).unwrap()
    );
    assert!(
        telchar::persistence::bind_microvm_launch(
            database.url(),
            &intent.intent_id,
            "microvm-exact"
        )
        .unwrap()
    );
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        database.url(),
        Duration::from_secs(30),
    )
    .unwrap();
    let pending = telchar::microvm::rendezvous::PendingConnections::new(2, 1).unwrap();
    for (id, accepted) in [
        ("microvm-wrong", false),
        ("microvm-exact", true),
        ("microvm-exact", false),
    ] {
        let gateway = TcpListener::bind("127.0.0.1:0").unwrap();
        let gateway_address = gateway.local_addr().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::scope(|scope| {
            let admission = scope.spawn(|| {
                let (stream, _) = gateway.accept().unwrap();
                let result = telchar::microvm::rendezvous::accept_session(
                    stream,
                    &owner,
                    Instant::now() + Duration::from_secs(2),
                    pending.reserve().unwrap(),
                );
                match result {
                    Ok(mut socket) => {
                        socket
                            .send(tungstenite::Message::Binary(b"admitted".to_vec().into()))
                            .unwrap();
                        true
                    }
                    Err(_) => false,
                }
            });
            let terminator = scope.spawn(move || -> Result<(), String> {
                let (stream, _) = listener.accept().map_err(|error| error.to_string())?;
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
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
                let mut incoming = tungstenite::accept_hdr(
                    tls,
                    |_: &tungstenite::handshake::server::Request,
                     mut response: tungstenite::handshake::server::Response| {
                        response.headers_mut().insert(
                            "sec-websocket-protocol",
                            "telchar-microvm-v1".parse().unwrap(),
                        );
                        Ok(response)
                    },
                )
                .map_err(|error| error.to_string())?;
                let mut request = format!("ws://{gateway_address}/microvm")
                    .into_client_request()
                    .unwrap();
                request.headers_mut().insert(
                    "sec-websocket-protocol",
                    "telchar-microvm-v1".parse().unwrap(),
                );
                let stream = TcpStream::connect(gateway_address).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let (mut outgoing, _) =
                    tungstenite::client(request, stream).map_err(|error| error.to_string())?;
                outgoing
                    .send(incoming.read().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
                incoming
                    .send(outgoing.read().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
                Ok(())
            });
            let mut roots = rustls::RootCertStore::empty();
            roots
                .add(include_bytes!("fixtures/microvm-cert.der").to_vec().into())
                .unwrap();
            let tls = rustls::ClientConfig::builder_with_provider(
                rustls::crypto::ring::default_provider().into(),
            )
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
            let mut socket = telchar_nomad_worker::microvm::connect(
                &format!("wss://localhost:{}/microvm", address.port()),
                address,
                Arc::new(tls),
                Instant::now() + Duration::from_secs(3),
                &intent.intent_id,
                id,
                capability.bearer(),
            )
            .unwrap();
            let received = socket.read();
            assert_eq!(received.is_ok(), accepted);
            if let Ok(message) = received {
                assert_eq!(message.into_data().as_ref(), b"admitted");
            }
            assert_eq!(admission.join().unwrap(), accepted);
            assert_eq!(terminator.join().unwrap().is_ok(), accepted);
        });
    }
}
