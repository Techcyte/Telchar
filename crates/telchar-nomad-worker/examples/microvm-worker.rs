//! Disposable execution-authenticated worker fixture, not a selectable backend.
use std::io;
use std::time::Duration;

use serde::Deserialize;
use tungstenite::client::IntoClientRequest;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Execution {
    endpoint: String,
    credential: String,
    certificate: String,
    execution: String,
    #[serde(rename = "microvmId")]
    microvm_id: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("execution file required")?;
    let execution: Execution = serde_json::from_reader(std::fs::File::open(path)?)?;
    let endpoint = url::Url::parse(&execution.endpoint)?;
    if endpoint.scheme() != "wss" || !endpoint.username().is_empty() || endpoint.password().is_some() {
        return Err("fixture requires WSS".into());
    }
    let mut request = execution.endpoint.into_client_request()?;
    for (name, value) in [
        ("authorization", format!("Bearer {}", execution.credential)),
        ("x-telchar-execution", execution.execution),
        ("x-telchar-microvm", execution.microvm_id),
    ] {
        request.headers_mut().insert(
            tungstenite::http::HeaderName::from_bytes(name.as_bytes())?,
            tungstenite::http::HeaderValue::from_str(&value)?,
        );
    }
    let certificate = execution.certificate.as_bytes().chunks(2).map(|pair| {
        let text = std::str::from_utf8(pair).map_err(io::Error::other)?;
        u8::from_str_radix(text, 16).map_err(io::Error::other)
    }).collect::<io::Result<Vec<_>>>()?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls::pki_types::CertificateDer::from(certificate))?;
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let stream = std::net::TcpStream::connect((
        endpoint.host_str().ok_or("receiver host required")?,
        endpoint.port_or_known_default().ok_or("receiver port required")?,
    ))?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    let (mut socket, _) = tungstenite::client_tls_with_config(
        request, stream, None,
        Some(tungstenite::Connector::Rustls(std::sync::Arc::new(tls))),
    )?;
    let stream = match socket.get_mut() {
        tungstenite::stream::MaybeTlsStream::Plain(stream) => stream,
        tungstenite::stream::MaybeTlsStream::Rustls(stream) => &mut stream.sock,
        _ => return Err("unsupported transport".into()),
    };
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    let mut session = telchar_nomad_worker::receive_authenticated_manifest(
        socket, 8 * 1024 * 1024, 256 * 1024, Duration::from_secs(540),
    )?;
    let store = "unix:///nix/var/nix/daemon-socket/socket";
    let requested = session.resolve_inputs(store)?;
    session.import_requested_inputs(store, &requested)?;
    let result = session.build(store)?;
    session.return_outputs(store, &result)?;
    println!("microvm outbound build transfer succeeded");
    Ok(())
}
