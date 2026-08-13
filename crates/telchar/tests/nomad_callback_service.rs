use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use telchar::config::ServiceConfig;
use telchar::nomad_callback_service::NomadCallbackService;

mod support;

#[test]
fn shutdown_stops_accepting_and_force_closes_after_bounded_drain() {
    let root = std::env::temp_dir().join(format!(
        "telchar-callback-service-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("fixture creates");
    let config_path = root.join("telchar.toml");
    std::fs::write(
        &config_path,
        r#"
[nomad_callback]
bind = "127.0.0.1:17443"
public_url = "ws://127.0.0.1:17443/callback"
maximum_connections = 1
maximum_header_bytes = 16384
maximum_body_bytes = 65536
authentication_request_timeout_seconds = 30
shutdown_drain_timeout_seconds = 1
maximum_jwks_bytes = 1048576
maximum_retained_nonces = 65536
"#,
    )
    .expect("configuration writes");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load().expect("configuration loads");
    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let address = listener.local_addr().expect("address reads");
    let database = support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("database migrates");
    let mut service = NomadCallbackService::start(
        listener,
        config.nomad_callback().clone(),
        database.url().to_owned(),
        vec![],
        Duration::from_secs(60),
    )
    .expect("service starts");
    let mut client = TcpStream::connect(address).expect("client connects");
    client
        .write_all(
            b"GET /callback HTTP/1.1\r\nHost: gateway\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: telchar-nomad-transfer-v1\r\n\r\n",
        )
        .expect("handshake writes");
    let accepted_deadline = Instant::now() + Duration::from_secs(1);
    while service
        .active_connections()
        .expect("active connections reads")
        == 0
    {
        assert!(
            Instant::now() < accepted_deadline,
            "callback was not accepted"
        );
        thread::yield_now();
    }

    let started = Instant::now();
    service.shutdown().expect("service shuts down");
    assert!(started.elapsed() < Duration::from_secs(3));

    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("timeout sets");
    let mut byte = [0_u8; 1];
    match client.read(&mut byte) {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        result => panic!("socket remained open: {result:?}"),
    }
    assert!(TcpStream::connect(address).is_err());
    let _ = client.shutdown(Shutdown::Both);
    std::fs::remove_dir_all(root).expect("fixture removes");
}
