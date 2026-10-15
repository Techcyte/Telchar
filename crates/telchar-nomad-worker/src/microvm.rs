//! Outbound MicroVM bootstrap using operator-supplied TLS trust.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Instant;

use tungstenite::client::IntoClientRequest;

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
}
