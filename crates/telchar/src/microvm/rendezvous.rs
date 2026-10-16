//! Authenticated MicroVM sessions behind operator-owned TLS termination.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tungstenite::handshake::server::{Request, Response};
use tungstenite::protocol::{Message, WebSocket, WebSocketConfig};

use super::ProviderError;
use crate::service::singleton_ownership::SingletonOwnership;

pub struct PendingConnections {
    maximum: usize,
    maximum_backend: usize,
    counts: std::sync::Mutex<PendingCounts>,
}

#[derive(Default)]
struct PendingCounts {
    total: usize,
    backends: std::collections::HashMap<String, usize>,
}

pub struct PendingConnection<'a> {
    limits: &'a PendingConnections,
    backend: Option<String>,
}

impl PendingConnections {
    pub fn new(maximum: usize, maximum_backend: usize) -> Result<Self, ProviderError> {
        if maximum == 0 || maximum_backend == 0 || maximum_backend > maximum {
            return Err(ProviderError);
        }
        Ok(Self {
            maximum,
            maximum_backend,
            counts: std::sync::Mutex::new(PendingCounts::default()),
        })
    }

    pub fn reserve(&self) -> Result<PendingConnection<'_>, ProviderError> {
        let mut counts = self.counts.lock().map_err(|_| ProviderError)?;
        if counts.total >= self.maximum {
            return Err(ProviderError);
        }
        counts.total += 1;
        Ok(PendingConnection {
            limits: self,
            backend: None,
        })
    }
}

impl PendingConnection<'_> {
    pub fn authenticate(&mut self, backend: &str) -> Result<(), ProviderError> {
        if self.backend.is_some() || backend.is_empty() || backend.len() > 256 {
            return Err(ProviderError);
        }
        let mut counts = self.limits.counts.lock().map_err(|_| ProviderError)?;
        let count = counts.backends.entry(backend.to_owned()).or_default();
        if *count >= self.limits.maximum_backend {
            return Err(ProviderError);
        }
        *count += 1;
        self.backend = Some(backend.to_owned());
        Ok(())
    }
}

impl Drop for PendingConnection<'_> {
    fn drop(&mut self) {
        if let Ok(mut counts) = self.limits.counts.lock() {
            counts.total -= 1;
            if let Some(backend) = &self.backend
                && let Some(count) = counts.backends.get_mut(backend)
            {
                *count -= 1;
                if *count == 0 {
                    counts.backends.remove(backend);
                }
            }
        }
    }
}

pub struct SessionStream {
    inner: TcpStream,
    deadline: Instant,
    header_bytes: Option<usize>,
}

impl SessionStream {
    pub(crate) fn interrupt_handle(&self) -> std::io::Result<TcpStream> {
        self.inner.try_clone()
    }

    fn verify_waiting_peer(&self) -> Result<(), ProviderError> {
        self.inner
            .set_nonblocking(true)
            .map_err(|_| ProviderError)?;
        let mut byte = [0];
        let observation = self.inner.peek(&mut byte);
        self.inner
            .set_nonblocking(false)
            .map_err(|_| ProviderError)?;
        match observation {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
            _ => Err(ProviderError),
        }
    }

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
    expected_intent: &str,
    deadline: Instant,
    mut pending: PendingConnection<'_>,
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
    if bootstrap.intent_id != expected_intent
        || bootstrap.intent_id.is_empty()
        || bootstrap.intent_id.len() > 128
        || bootstrap.provider_id.is_empty()
        || bootstrap.provider_id.len() > 256
    {
        return Err(ProviderError);
    }
    let verifier = super::bearer_verifier(&bootstrap.bearer).ok_or(ProviderError)?;
    let backend = crate::persistence::microvm_session_backend(
        owner.database(),
        &bootstrap.intent_id,
        &verifier,
    )
    .map_err(|_| ProviderError)?
    .ok_or(ProviderError)?;
    pending.authenticate(&backend)?;
    loop {
        if Instant::now() >= deadline {
            return Err(ProviderError);
        }
        socket.get_ref().verify_waiting_peer()?;
        if super::admit_session(
            owner,
            &bootstrap.intent_id,
            &bootstrap.provider_id,
            &bootstrap.bearer,
        )? {
            return Ok(socket);
        }
        if !crate::persistence::microvm_session_awaits_binding(
            owner.database(),
            &bootstrap.intent_id,
            &verifier,
        )
        .map_err(|_| ProviderError)?
        {
            // Binding can commit between the admission attempt and pending observation.
            socket.get_ref().verify_waiting_peer()?;
            if super::admit_session(
                owner,
                &bootstrap.intent_id,
                &bootstrap.provider_id,
                &bootstrap.bearer,
            )? {
                return Ok(socket);
            }
            return Err(ProviderError);
        }
        std::thread::sleep(
            Duration::from_millis(25).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}
