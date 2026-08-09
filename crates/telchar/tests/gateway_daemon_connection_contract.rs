use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nix_worker_protocol::{
    WorkerTrust, CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_LAST,
};

const STORE_PATH: &[u8] = b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-output";
const NAR_HASH: &[u8] = b"6c2be2f12a168605ebcba3782286c38eaf3f5d787b7a8bb24a540d2267ff68e1";
use telchar::store_daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

struct SocketFixture {
    root: PathBuf,
    socket: PathBuf,
}

impl SocketFixture {
    fn create() -> Self {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "telchar-gateway-daemon-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("socket fixture directory creates");
        let socket = root.join("gateway.sock");
        Self { root, socket }
    }

    fn endpoint(&self) -> GatewayStoreEndpoint {
        GatewayStoreEndpoint::parse(&format!("unix://{}", self.socket.display()))
            .expect("fixture endpoint parses")
    }
}

impl Drop for SocketFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn integer(output: &mut impl Write, value: u64) {
    output
        .write_all(&value.to_le_bytes())
        .expect("worker integer writes");
}

fn read_integer(input: &mut impl Read) -> u64 {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes).expect("worker integer reads");
    u64::from_le_bytes(bytes)
}

fn byte_string(output: &mut impl Write, value: &[u8]) {
    integer(output, value.len() as u64);
    output.write_all(value).expect("worker string writes");
    output
        .write_all(&vec![0; (8 - value.len() % 8) % 8])
        .expect("worker padding writes");
}

fn complete_handshake(stream: &mut std::os::unix::net::UnixStream, trust: u64) {
    assert_eq!(read_integer(stream), CLIENT_WORKER_MAGIC);
    assert_eq!(read_integer(stream), LATEST_WORKER_VERSION.to_wire());
    integer(stream, SERVER_WORKER_MAGIC);
    integer(stream, LATEST_WORKER_VERSION.to_wire());
    stream.flush().expect("server greeting flushes");
    assert_eq!(read_integer(stream), 0);
    integer(stream, 0);
    stream.flush().expect("feature response flushes");
    assert_eq!(read_integer(stream), 0);
    assert_eq!(read_integer(stream), 0);
    byte_string(stream, b"2.34.8");
    integer(stream, trust);
    integer(stream, STDERR_LAST);
    stream.flush().expect("post-handshake flushes");
}

#[test]
fn gateway_connection_delegates_typed_path_queries() {
    let fixture = SocketFixture::create();
    let listener = UnixListener::bind(&fixture.socket).expect("listener binds");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("connection accepts");
        complete_handshake(&mut stream, 1);

        assert_eq!(read_integer(&mut stream), 1);
        assert_eq!(read_byte_string(&mut stream), STORE_PATH);
        integer(&mut stream, STDERR_LAST);
        integer(&mut stream, 1);
        stream.flush().expect("validity response flushes");

        assert_eq!(read_integer(&mut stream), 26);
        assert_eq!(read_byte_string(&mut stream), STORE_PATH);
        integer(&mut stream, STDERR_LAST);
        integer(&mut stream, 1);
        byte_string(&mut stream, b"");
        byte_string(&mut stream, NAR_HASH);
        integer(&mut stream, 0);
        integer(&mut stream, 42);
        integer(&mut stream, 136);
        integer(&mut stream, 0);
        integer(&mut stream, 0);
        byte_string(&mut stream, b"");
        stream.flush().expect("path info response flushes");
    });
    let mut connection = GatewayStoreConnection::connect(&fixture.endpoint())
        .expect("gateway connection establishes");

    assert!(connection.is_valid_path(STORE_PATH).unwrap());
    let info = connection.query_path_info(STORE_PATH).unwrap().unwrap();
    assert_eq!(info.nar_hash_hex().as_bytes(), NAR_HASH);
    assert_eq!(info.registration_time(), 42);
    assert_eq!(info.nar_size(), 136);

    drop(connection);
    server.join().expect("server exits");
}

fn read_byte_string(input: &mut impl Read) -> Vec<u8> {
    let length = read_integer(input) as usize;
    let padding = (8 - length % 8) % 8;
    let mut value = vec![0; length + padding];
    input.read_exact(&mut value).expect("worker string reads");
    assert!(value[length..].iter().all(|byte| *byte == 0));
    value.truncate(length);
    value
}

#[test]
fn gateway_store_endpoint_accepts_absolute_unix_socket() {
    let fixture = SocketFixture::create();
    GatewayStoreEndpoint::parse(&format!("unix://{}", fixture.socket.display()))
        .expect("absolute Unix endpoint is accepted");
}

#[test]
fn endpoint_rejects_fallback_and_parameter_forms() {
    for value in [
        "",
        "unix://",
        "unix://relative/socket",
        "unix://host/absolute/socket",
        "unix:///tmp/gateway.sock?trusted=true",
        "unix:///tmp/gateway.sock#fragment",
        "unix:///tmp/gateway\0.sock",
        "local",
        "daemon",
        "ssh-ng://builder",
        "tcp://127.0.0.1:1234",
        "other:///tmp/gateway.sock",
    ] {
        assert!(
            GatewayStoreEndpoint::parse(value).is_err(),
            "unsupported endpoint accepted: {value:?}"
        );
    }

    let non_unicode =
        std::ffi::OsString::from_vec(vec![b'u', b'n', b'i', b'x', b':', b'/', b'/', b'/', 0xff]);
    assert!(GatewayStoreEndpoint::parse_os(&non_unicode).is_err());
}

#[test]
fn configured_endpoint_negotiates_typed_profile_and_drop_closes_socket() {
    let fixture = SocketFixture::create();
    let listener = UnixListener::bind(&fixture.socket).expect("private listener binds");
    let (closed_sender, closed_receiver) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("connection arrives");
        complete_handshake(&mut stream, 1);
        let mut byte = [0; 1];
        closed_sender
            .send(stream.read(&mut byte).expect("peer closure reads"))
            .expect("closure evidence sends");
    });

    let connection = GatewayStoreConnection::connect(&fixture.endpoint())
        .expect("configured daemon connection succeeds");
    assert_eq!(connection.profile().version, LATEST_WORKER_VERSION);
    assert_eq!(connection.profile().trust, WorkerTrust::Trusted);
    assert!(connection.profile().capabilities.root_registration);
    drop(connection);

    assert_eq!(
        closed_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("peer observes closure"),
        0
    );
    server.join().expect("server joins");
}

#[test]
fn missing_socket_fails_without_fallback_and_redacts_path() {
    let fixture = SocketFixture::create();
    let endpoint = fixture.endpoint();
    let error = GatewayStoreConnection::connect(&endpoint)
        .err()
        .expect("missing socket fails");

    assert_eq!(error.to_string(), "gateway Nix daemon connection failed");
    assert!(!error.to_string().contains(fixture.root.to_str().unwrap()));
}

#[test]
fn malformed_handshake_closes_socket_and_redacts_peer_bytes() {
    let fixture = SocketFixture::create();
    let listener = UnixListener::bind(&fixture.socket).expect("private listener binds");
    let (closed_sender, closed_receiver) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("connection arrives");
        assert_eq!(read_integer(&mut stream), CLIENT_WORKER_MAGIC);
        assert_eq!(read_integer(&mut stream), LATEST_WORKER_VERSION.to_wire());
        integer(&mut stream, 0xdead_beef);
        integer(&mut stream, LATEST_WORKER_VERSION.to_wire());
        stream.flush().expect("malformed greeting flushes");
        let mut byte = [0; 1];
        let closed = match stream.read(&mut byte) {
            Ok(0) => true,
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => true,
            _ => false,
        };
        closed_sender.send(closed).expect("closure evidence sends");
    });

    let error = GatewayStoreConnection::connect(&fixture.endpoint())
        .err()
        .expect("malformed handshake fails");

    assert_eq!(error.to_string(), "gateway Nix daemon connection failed");
    assert!(!error.to_string().contains("dead"));
    assert!(closed_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("peer observes closure"));
    server.join().expect("server joins");
}

#[test]
fn handshake_timeout_closes_socket() {
    let fixture = SocketFixture::create();
    let listener = UnixListener::bind(&fixture.socket).expect("private listener binds");
    let (closed_sender, closed_receiver) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("connection arrives");
        assert_eq!(read_integer(&mut stream), CLIENT_WORKER_MAGIC);
        assert_eq!(read_integer(&mut stream), LATEST_WORKER_VERSION.to_wire());
        let mut byte = [0; 1];
        closed_sender
            .send(stream.read(&mut byte).expect("peer closure reads"))
            .expect("closure evidence sends");
    });

    let error = GatewayStoreConnection::connect_with_timeout(
        &fixture.endpoint(),
        Duration::from_millis(50),
    )
    .err()
    .expect("incomplete handshake times out");

    assert_eq!(error.to_string(), "gateway Nix daemon connection failed");
    assert_eq!(
        closed_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("peer observes closure"),
        0
    );
    server.join().expect("server joins");
}

#[test]
fn endpoint_type_does_not_expose_configured_path() {
    let fixture = SocketFixture::create();
    let endpoint = fixture.endpoint();
    let rendered = format!("{endpoint:?}");

    assert!(!rendered.contains(Path::new(&fixture.socket).to_str().unwrap()));
}
