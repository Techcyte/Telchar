//! Tests nomad callback service contracts and failure boundaries, including shutdown stops accepting and force closes after bounded drain.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use telchar::nomad::callback_service::NomadCallbackService;
use telchar::service::config::ServiceConfig;

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
    let hmac_secret = root.join("hmac-secret");
    std::fs::write(&hmac_secret, "callback-secret\n").expect("HMAC secret writes");
    std::fs::set_permissions(
        &hmac_secret,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .expect("HMAC secret permissions set");
    let config_path = root.join("telchar.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[backends.nomad_callback]
bind = "127.0.0.1:17443"
public_url = "ws://127.0.0.1:17443/callback"
maximum_connections = 1
maximum_header_bytes = 16384
maximum_body_bytes = 65536
authentication_request_timeout_seconds = 30
shutdown_drain_timeout_seconds = 1
maximum_jwks_bytes = 1048576
maximum_retained_nonces = 65536

[[backends.nomad]]
[backends.nomad.nomad-primary]
system = "x86_64-linux"
supported_features = []
maximum_concurrent_builds = 1
endpoint = "http://nomad.internal:4646"
namespace = "telchar"
driver = "raw_exec"
job_name_scope = "telchar"
poll_interval_seconds = 1
runtime_limit_seconds = 60

[backends.nomad.nomad-primary.driver_config]
command = "/bin/true"

[backends.nomad.nomad-primary.resources]
cpu_mhz = 100
memory_mb = 128
disk_mb = 128

[backends.nomad.nomad-primary.transfer_authentication]
mode = "hmac"
key_id = "callback-test"
secret_file = "{}"

[backends.nomad.nomad-primary.store]
mode = "daemon"
uri = "unix:///definitely-missing/telchar-worker.sock"

[backends.nomad.nomad-primary.transfer_limits]
maximum_manifest_paths = 1
maximum_manifest_bytes = 1024
maximum_input_nar_bytes = 1024
maximum_total_input_bytes = 1024
maximum_output_nar_bytes = 1024
maximum_total_output_bytes = 1024
maximum_frame_metadata_bytes = 1024
stream_buffer_bytes = 1024
maximum_live_log_chunk_bytes = 1024
live_log_queue_bytes = 1024
transfer_idle_timeout_seconds = 60
setup_timeout_seconds = 60
output_collection_timeout_seconds = 60
maximum_connection_lifetime_seconds = 3600
authentication_lifetime_seconds = 60
clock_skew_seconds = 5
nonce_retention_seconds = 120
reconnect_timeout_seconds = 60
maximum_diagnostic_bytes = 1024
"#,
            hmac_secret.display()
        ),
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
        config
            .nomad_callback()
            .expect("Nomad callback is configured")
            .clone(),
        telchar::persistence::Database::connect(database.url()).expect("database connects"),
        config.nomad_backends().to_vec(),
        telchar::store::daemon::GatewayStoreEndpoint::parse(
            "unix:///definitely-missing/telchar-gateway.sock",
        )
        .expect("gateway endpoint is valid"),
        Duration::from_secs(60),
        std::sync::Arc::new(telchar::shared_build::SharedBuildRegistry::new()),
    )
    .expect("service starts");
    let mut client = TcpStream::connect(address).expect("client connects");
    let websocket_key = String::from_utf8(vec![
        100, 71, 104, 108, 73, 72, 78, 104, 98, 88, 66, 115, 90, 83, 66, 117, 98, 50, 53, 106, 90,
        81, 61, 61,
    ])
    .expect("WebSocket key is UTF-8");
    write!(
        client,
        "GET /callback HTTP/1.1\r\nHost: gateway\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {websocket_key}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: telchar-nomad-transfer-v1\r\n\r\n"
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
    let mut response = Vec::new();
    match client.read_to_end(&mut response) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        result => panic!("socket remained open: {result:?}"),
    }
    assert!(
        response.starts_with(b"HTTP/1.1 101"),
        "callback upgrade was not accepted: {}",
        String::from_utf8_lossy(&response)
    );
    assert!(TcpStream::connect(address).is_err());
    let _ = client.shutdown(Shutdown::Both);
    std::fs::remove_dir_all(root).expect("fixture removes");
}
