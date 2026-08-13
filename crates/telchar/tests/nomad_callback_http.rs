use std::io::{Read, Write};

use telchar::nomad_callback_http::{accept_connection, CallbackHttpLimits};

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
    format!(
        "GET {path} HTTP/1.1\r\nHost: gateway\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: {protocol}\r\n\r\n"
    )
}

#[test]
fn accepts_bounded_websocket_upgrade_with_exact_subprotocol() {
    let stream = FragmentedStream::new(handshake("/callback", "telchar-nomad-transfer-v1"), 128);
    let socket =
        accept_connection(stream, CallbackHttpLimits::new(1024, 4096)).expect("WebSocket accepts");
    let stream = socket.into_inner();
    assert!(String::from_utf8(stream.output)
        .expect("response is UTF-8")
        .starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
}

#[test]
fn rejects_wrong_subprotocol_and_oversized_headers() {
    for request in [
        handshake("/callback", "foreign"),
        format!(
            "GET /callback HTTP/1.1\r\nHost: gateway\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: telchar-nomad-transfer-v1\r\nX-Fill: {}\r\n\r\n",
            "a".repeat(1100)
        ),
    ] {
        let stream = FragmentedStream::new(request, 11);
        assert!(accept_connection(stream, CallbackHttpLimits::new(1024, 8)).is_err());
    }
}
