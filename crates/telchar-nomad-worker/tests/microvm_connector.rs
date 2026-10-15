use std::io;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn wss_connector_verifies_trust_hostname_and_separate_bootstrap() {
    for (host, trusted) in [
        ("localhost", true),
        ("wrong.invalid", true),
        ("localhost", false),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || -> io::Result<()> {
            let (stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            stream.set_write_timeout(Some(Duration::from_secs(2)))?;
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
            let mut socket = tungstenite::accept_hdr(
                tls,
                |request: &tungstenite::handshake::server::Request,
                 mut response: tungstenite::handshake::server::Response| {
                    assert_eq!(request.uri().path(), "/microvm");
                    assert_eq!(
                        request.headers()["sec-websocket-protocol"],
                        "telchar-microvm-v1"
                    );
                    response.headers_mut().insert(
                        "sec-websocket-protocol",
                        "telchar-microvm-v1".parse().unwrap(),
                    );
                    Ok(response)
                },
            )
            .map_err(io::Error::other)?;
            let message = socket.read().map_err(io::Error::other)?;
            assert!(message.is_binary());
            let bootstrap: serde_json::Value =
                serde_json::from_slice(&message.into_data()).unwrap();
            assert_eq!(
                bootstrap,
                serde_json::json!({"intent_id":"intent", "provider_id":"microvm-exact", "bearer":"secret"})
            );
            Ok(())
        });
        let mut roots = rustls::RootCertStore::empty();
        if trusted {
            roots
                .add(include_bytes!("fixtures/microvm-cert.der").to_vec().into())
                .unwrap();
        }
        let tls = rustls::ClientConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let result = telchar_nomad_worker::microvm::connect(
            &format!("wss://{host}:{}/microvm", address.port()),
            address,
            Arc::new(tls),
            Instant::now() + Duration::from_secs(2),
            "intent",
            "microvm-exact",
            "secret",
        );
        assert_eq!(result.is_ok(), host == "localhost" && trusted);
        assert_eq!(
            server.join().unwrap().is_ok(),
            host == "localhost" && trusted
        );
    }
}
