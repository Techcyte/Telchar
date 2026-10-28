use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn slow_upgrade_cannot_extend_setup_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        let config = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![include_bytes!("fixtures/microvm-cert.der").to_vec().into()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(
                include_bytes!("fixtures/microvm-key.der").to_vec().into(),
            ),
        )
        .expect("certificate");
        let mut tls = rustls::StreamOwned::new(
            rustls::ServerConnection::new(Arc::new(config)).expect("TLS"),
            stream,
        );
        let mut byte = [0];
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            tls.read_exact(&mut byte).expect("upgrade request");
            request.push(byte[0]);
        }
        for byte in b"HTTP/1.1 101 Switching Protocols\r\n" {
            if tls.write_all(&[*byte]).and_then(|_| tls.flush()).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(60));
        }
    });
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(include_bytes!("fixtures/microvm-cert.der").to_vec().into())
        .expect("root");
    let tls = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .expect("versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    let started = Instant::now();
    let result = telchar_nomad_worker::microvm::connect(
        &format!("wss://localhost:{}/microvm", address.port()),
        address,
        Arc::new(tls),
        started + Duration::from_millis(300),
        "intent",
        "microvm-exact",
        "secret",
    );
    let elapsed = started.elapsed();
    assert!(result.is_err());
    server.join().expect("server");
    assert!(
        elapsed < Duration::from_millis(900),
        "absolute setup budget exceeded: {elapsed:?}"
    );
}
