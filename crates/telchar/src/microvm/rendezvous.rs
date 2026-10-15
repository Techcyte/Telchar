//! Authenticated MicroVM sessions behind operator-owned TLS termination.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tungstenite::handshake::server::{Request, Response};
use tungstenite::protocol::{Message, WebSocket, WebSocketConfig};

use super::ProviderError;
use crate::service::singleton_ownership::SingletonOwnership;

pub struct SessionStream {
    inner: TcpStream,
    deadline: Instant,
    header_bytes: Option<usize>,
}

impl SessionStream {
    fn remaining(&self) -> std::io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "MicroVM session deadline expired",
                )
            })
    }
}

impl Read for SessionStream {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        self.inner.set_read_timeout(Some(self.remaining()?))?;
        if output.is_empty() {
            return Ok(0);
        }
        if let Some(bytes) = self.header_bytes.as_mut() {
            if *bytes >= 8192 {
                return Err(std::io::Error::other("MicroVM headers exceed limit"));
            }
            let maximum = output.len().min(8192 - *bytes).min(1024);
            let length = self.inner.read(&mut output[..maximum])?;
            *bytes += length;
            return Ok(length);
        }
        self.inner.read(output)
    }
}

impl Write for SessionStream {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        self.inner.set_write_timeout(Some(self.remaining()?))?;
        self.inner.write(input)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.set_write_timeout(Some(self.remaining()?))?;
        self.inner.flush()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bootstrap {
    intent_id: String,
    provider_id: String,
    bearer: String,
}

pub fn accept_session(
    stream: TcpStream,
    owner: &SingletonOwnership,
    deadline: Instant,
) -> Result<WebSocket<SessionStream>, ProviderError> {
    owner.verify().map_err(|_| ProviderError)?;
    let stream = SessionStream {
        inner: stream,
        deadline,
        header_bytes: Some(0),
    };
    #[allow(clippy::result_large_err)]
    let upgrade = |request: &Request, mut response: Response| {
        if request.uri().path() != "/microvm"
            || request.uri().query().is_some()
            || request
                .headers()
                .get("sec-websocket-protocol")
                .and_then(|value| value.to_str().ok())
                != Some("telchar-microvm-v1")
        {
            let mut rejected = tungstenite::handshake::server::ErrorResponse::new(None);
            *rejected.status_mut() = tungstenite::http::StatusCode::BAD_REQUEST;
            return Err(rejected);
        }
        response.headers_mut().insert(
            "sec-websocket-protocol",
            tungstenite::http::HeaderValue::from_static("telchar-microvm-v1"),
        );
        Ok(response)
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(4096))
        .max_frame_size(Some(4096));
    let mut socket = tungstenite::accept_hdr_with_config(stream, upgrade, Some(config))
        .map_err(|_| ProviderError)?;
    socket.get_mut().header_bytes = None;
    let Message::Binary(message) = socket.read().map_err(|_| ProviderError)? else {
        return Err(ProviderError);
    };
    let bootstrap: Bootstrap = serde_json::from_slice(&message).map_err(|_| ProviderError)?;
    if bootstrap.intent_id.is_empty()
        || bootstrap.intent_id.len() > 128
        || bootstrap.provider_id.is_empty()
        || bootstrap.provider_id.len() > 256
        || !super::admit_session(
            owner,
            &bootstrap.intent_id,
            &bootstrap.provider_id,
            &bootstrap.bearer,
        )?
    {
        return Err(ProviderError);
    }
    Ok(socket)
}
