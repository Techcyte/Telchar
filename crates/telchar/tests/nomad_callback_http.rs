//! Tests nomad callback http contracts and failure boundaries, including handshake.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use tungstenite::Message;

use telchar::nomad::callback_http::{CallbackHttpLimits, accept_connection};

struct FragmentedStream {
    input: Vec<u8>,
    offset: usize,
    output: Vec<u8>,
    maximum_read: usize,
}

impl FragmentedStream {
    fn new(input: impl Into<Vec<u8>>, maximum_read: usize) -> Self {
        Self {
            input: input.into(),
            offset: 0,
            output: Vec::new(),
            maximum_read,
        }
    }
}

impl Read for FragmentedStream {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let available = self.input.len().saturating_sub(self.offset);
        let length = available.min(output.len()).min(self.maximum_read);
        output[..length].copy_from_slice(&self.input[self.offset..self.offset + length]);
        self.offset += length;
        Ok(length)
    }
}

impl Write for FragmentedStream {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        self.output.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn handshake(path: &str, protocol: &str) -> String {
    let websocket_key = String::from_utf8(vec![
        100, 71, 104, 108, 73, 72, 78, 104, 98, 88, 66, 115, 90, 83, 66, 117, 98, 50, 53, 106, 90,
        81, 61, 61,
    ])
    .expect("WebSocket key is UTF-8");
    format!(
        "GET {path} HTTP/1.1\r\nHost: gateway\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {websocket_key}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: {protocol}\r\n\r\n"
    )
}

#[test]
fn accepts_bounded_websocket_upgrade_with_exact_subprotocol() {
    let request = handshake("/callback", "telchar-nomad-transfer-v1").replace(
        "\r\n\r\n",
        "\r\ntraceparent: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01\r\ntracestate: vendor=value\r\n\r\n",
    );
    let stream = FragmentedStream::new(request, 128);
    let mut socket =
        accept_connection(stream, CallbackHttpLimits::new(1024, 4096)).expect("WebSocket accepts");
    assert_eq!(
        socket
            .trace_context()
            .trace_id()
            .expect("trace ID exists")
            .to_string(),
        "4bf92f3577b34da6a3ce929d0e0e4736"
    );
    socket.set_maximum_message_bytes(8192);
    let stream = socket.into_inner();
    assert!(
        String::from_utf8(stream.output)
            .expect("response is UTF-8")
            .starts_with("HTTP/1.1 101 Switching Protocols\r\n")
    );
}

#[test]
fn sends_ping_during_quiet_transfer_and_accepts_matching_pong() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let address = listener.local_addr().expect("address reads");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("connection accepts");
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .expect("read timeout sets");
        let mut socket = accept_connection(stream, CallbackHttpLimits::new(1024, 4096))
            .expect("WebSocket accepts");
        socket.configure_keepalive(
            Duration::from_millis(100),
            Instant::now() + Duration::from_secs(2),
        );
        socket.read_binary().expect("binary message reads")
    });
    let request = tungstenite::client::IntoClientRequest::into_client_request(format!(
        "ws://{address}/callback"
    ))
    .expect("request creates");
    let mut request = request;
    request.headers_mut().insert(
        "sec-websocket-protocol",
        tungstenite::http::HeaderValue::from_static("telchar-nomad-transfer-v1"),
    );
    let stream = TcpStream::connect(address).expect("client connects");
    let (mut client, _) = tungstenite::client(request, stream).expect("client upgrades");
    match client.read().expect("ping reads") {
        Message::Ping(payload) => client.send(Message::Pong(payload)).expect("pong sends"),
        message => panic!("expected ping, received {message:?}"),
    }
    client
        .send(Message::Binary(vec![1, 2, 3].into()))
        .expect("binary sends");
    assert_eq!(server.join().expect("server joins"), vec![1, 2, 3]);
}

#[test]
fn applies_updated_message_limit_to_outbound_binary_frames() {
    let stream = FragmentedStream::new(handshake("/callback", "telchar-nomad-transfer-v1"), 128);
    let mut socket =
        accept_connection(stream, CallbackHttpLimits::new(1024, 64)).expect("WebSocket accepts");
    socket.set_maximum_message_bytes(262_144 + 1024);

    socket
        .write_binary(vec![0; 262_144 + 512])
        .expect("bounded transfer frame writes");
}

#[test]
fn rejects_plain_http_health_probe_with_bounded_response() {
    for maximum_read in [11, 128] {
        let mut stream = FragmentedStream::new(
            "GET /callback HTTP/1.1\r\nHost: gateway\r\nX-Private: private-marker\r\n\r\n",
            maximum_read,
        );
        assert!(accept_connection(&mut stream, CallbackHttpLimits::new(1024, 8)).is_err());
        assert_eq!(
            stream.output,
            b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
        );
    }
}

#[test]
fn closes_incomplete_and_oversized_probes_without_response() {
    for request in [
        "GET /callback HTTP/1.1\r\nHost: gateway\r\n".to_owned(),
        format!(
            "GET /callback HTTP/1.1\r\nX-Fill: {}\r\n\r\n",
            "a".repeat(1100)
        ),
    ] {
        let mut stream = FragmentedStream::new(request, 128);
        assert!(accept_connection(&mut stream, CallbackHttpLimits::new(1024, 8)).is_err());
        assert!(stream.output.is_empty());
    }
}

#[test]
fn rejects_wrong_subprotocol_and_oversized_headers() {
    for request in [
        handshake("/callback", "foreign"),
        format!(
            "{}X-Fill: {}\r\n\r\n",
            handshake("/callback", "telchar-nomad-transfer-v1").trim_end_matches("\r\n\r\n"),
            "a".repeat(1100)
        ),
    ] {
        let mut stream = FragmentedStream::new(request, 11);
        assert!(accept_connection(&mut stream, CallbackHttpLimits::new(1024, 8)).is_err());
        let response = String::from_utf8(stream.output).expect("response is UTF-8");
        assert!(response.matches("HTTP/1.1").count() <= 1, "{response}");
    }
}
