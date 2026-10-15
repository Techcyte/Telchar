//! Outbound MicroVM bootstrap using operator-supplied TLS trust.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Instant;

use tungstenite::client::IntoClientRequest;

pub fn receive_session(
    mut socket: super::WorkerSocket,
    maximum_manifest_bytes: usize,
    transfer_chunk_bytes: usize,
    deadline: Instant,
) -> io::Result<super::WorkerSession> {
    if maximum_manifest_bytes == 0
        || maximum_manifest_bytes > super::MAXIMUM_MANIFEST_METADATA_BYTES
        || transfer_chunk_bytes == 0
        || transfer_chunk_bytes > super::MAXIMUM_NAR_CHUNK_BYTES
    {
        return Err(super::invalid("MicroVM session limits are invalid"));
    }
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| super::invalid("MicroVM session deadline expired"))?;
    let stream = match socket.get_ref() {
        tungstenite::stream::MaybeTlsStream::Plain(stream) => stream,
        tungstenite::stream::MaybeTlsStream::Rustls(stream) => &stream.sock,
        _ => return Err(super::invalid("MicroVM session transport is unsupported")),
    }
    .try_clone()?;
    let maximum_message_bytes = maximum_manifest_bytes
        .max(transfer_chunk_bytes)
        .checked_add(super::MAXIMUM_AUTHENTICATION_METADATA_BYTES + 64)
        .ok_or_else(|| super::invalid("MicroVM message limit is invalid"))?;
    socket.set_config(|config| {
        config.max_message_size = Some(maximum_message_bytes);
        config.max_frame_size = Some(maximum_message_bytes);
    });
    super::set_socket_timeouts(&mut socket, remaining)?;
    let monitor = SessionDeadline::start(stream, deadline)?;
    let mut session = super::receive_authenticated_manifest(
        socket,
        maximum_manifest_bytes,
        transfer_chunk_bytes,
        remaining,
    )?;
    if Instant::now() >= deadline {
        return Err(super::invalid("MicroVM session deadline expired"));
    }
    session.connection_deadline = deadline;
    session.deadline_monitor = Some(monitor);
    Ok(session)
}

pub(super) struct SessionDeadline {
    finished: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SessionDeadline {
    fn start(stream: TcpStream, deadline: Instant) -> io::Result<Self> {
        let (finished, completion) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("microvm-session-deadline".to_owned())
            .spawn(move || {
                if completion.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            })?;
        Ok(Self {
            finished,
            thread: Some(thread),
        })
    }
}

impl Drop for SessionDeadline {
    fn drop(&mut self) {
        let _ = self.finished.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn connect(
    endpoint: &str,
    address: SocketAddr,
    tls: Arc<rustls::ClientConfig>,
    deadline: Instant,
    intent_id: &str,
    provider_id: &str,
    bearer: &str,
) -> io::Result<super::WorkerSocket> {
    let failed = || io::Error::other("MicroVM bootstrap connection failed");
    let url = url::Url::parse(endpoint).map_err(|_| failed())?;
    if url.scheme() != "wss"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/microvm"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(failed());
    }
    let bootstrap = serde_json::to_vec(&serde_json::json!({
        "intent_id": intent_id, "provider_id": provider_id, "bearer": bearer,
    }))
    .map_err(|_| failed())?;
    if bootstrap.len() > 4096 {
        return Err(failed());
    }
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(failed)
    };
    let stream = TcpStream::connect_timeout(&address, remaining()?).map_err(|_| failed())?;
    stream
        .set_read_timeout(Some(remaining()?))
        .map_err(|_| failed())?;
    stream
        .set_write_timeout(Some(remaining()?))
        .map_err(|_| failed())?;
    let deadline_stream = stream.try_clone().map_err(|_| failed())?;
    let (finished, completion) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let watchdog = scope.spawn(move || {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if completion.recv_timeout(remaining) == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                let _ = deadline_stream.shutdown(std::net::Shutdown::Both);
            }
        });
        let result = (|| {
            let mut request = endpoint.into_client_request().map_err(|_| failed())?;
            request.headers_mut().insert(
                "sec-websocket-protocol",
                "telchar-microvm-v1".parse().map_err(|_| failed())?,
            );
            let (mut socket, response) = tungstenite::client_tls_with_config(
                request,
                stream,
                Some(
                    tungstenite::protocol::WebSocketConfig::default()
                        .max_message_size(Some(4096))
                        .max_frame_size(Some(4096)),
                ),
                Some(tungstenite::Connector::Rustls(tls)),
            )
            .map_err(|_| failed())?;
            if response
                .headers()
                .get("sec-websocket-protocol")
                .map(|value| value.as_bytes())
                != Some(b"telchar-microvm-v1".as_slice())
            {
                return Err(failed());
            }
            super::set_socket_timeouts(&mut socket, remaining()?).map_err(|_| failed())?;
            socket
                .send(tungstenite::Message::Binary(bootstrap.into()))
                .map_err(|_| failed())?;
            remaining()?;
            Ok(socket)
        })();
        let _ = finished.send(());
        watchdog.join().map_err(|_| failed())?;
        result
    })
}
