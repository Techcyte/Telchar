//! Disposable execution-authenticated WSS receiver fixture.

use rustls::pki_types::pem::PemObject;
use std::io::{self, Read, Write};
use std::path::Path;
use telchar::nomad::protocol::{
    Direction, Frame, FrameKind, InputManifest, PathSet, ProtocolLimits, TransferSession,
    decode_metadata, encode_metadata, read_frame, write_frame,
};

const METADATA: usize = 8 * 1024 * 1024;
const CHUNK: usize = 256 * 1024;

fn send<S: Read + Write>(socket: &mut tungstenite::WebSocket<S>, frame: &Frame) -> io::Result<()> {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, frame, ProtocolLimits::new(METADATA, CHUNK))?;
    socket.send(tungstenite::Message::Binary(bytes.into())).map_err(io::Error::other)
}

fn receive<S: Read + Write>(socket: &mut tungstenite::WebSocket<S>) -> io::Result<Frame> {
    let message = socket.read().map_err(io::Error::other)?;
    let tungstenite::Message::Binary(bytes) = message else {
        return Err(io::Error::other("expected binary transfer frame"));
    };
    let mut cursor = io::Cursor::new(bytes.as_ref());
    let frame = read_frame(&mut cursor, ProtocolLimits::new(METADATA, CHUNK))?;
    if cursor.position() != bytes.len() as u64 {
        return Err(io::Error::other("trailing transfer data"));
    }
    Ok(frame)
}

fn stream_input<S: Read + Write>(
    socket: &mut tungstenite::WebSocket<S>,
    entry: &telchar::nomad::protocol::PathManifestEntry,
    source: &mut impl Read,
    mut validate: impl FnMut(&Frame) -> io::Result<()>,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < entry.nar_size {
        let length = (entry.nar_size - offset).min(CHUNK as u64) as usize;
        let mut payload = vec![0; length];
        source.read_exact(&mut payload)?;
        let metadata = telchar::nomad::protocol::NarMetadata {
            path: entry.path.clone(), nar_hash: entry.nar_hash.clone(), nar_size: entry.nar_size,
            offset, final_chunk: offset + length as u64 == entry.nar_size,
        };
        let frame = Frame::new(FrameKind::InputNar, encode_metadata(&metadata, METADATA)?, payload);
        validate(&frame)?;
        send(socket, &frame)?;
        offset += length as u64;
    }
    Ok(())
}

fn exchange<S: Read + Write>(
    socket: &mut tungstenite::WebSocket<S>, manifest: InputManifest,
    directory: &Path, _store_uri: &str,
) -> io::Result<()> {
    let mut session = TransferSession::new(manifest.clone(), 10000, 10000, 1 << 30, 1 << 30, 1 << 30, 1 << 20, METADATA)?;
    send(socket, &Frame::new(FrameKind::InputManifest, encode_metadata(&manifest, METADATA)?, vec![]))?;
    let valid = receive(socket)?;
    session.accept(Direction::WorkerToGateway, valid)?;
    let request = receive(socket)?;
    let requested: PathSet = decode_metadata(request.metadata(), METADATA)?;
    session.accept(Direction::WorkerToGateway, request)?;
    for path in requested.paths {
        let (index, entry) = manifest.paths.iter().enumerate().find(|(_, entry)| entry.path == path)
            .ok_or_else(|| io::Error::other("unadmitted input"))?;
        let mut file = std::fs::File::open(directory.join(format!("{index}.nar")))?;
        stream_input(socket, entry, &mut file, |frame| session.accept(Direction::GatewayToWorker, frame.clone()))?;
    }
    loop {
        let frame = receive(socket)?;
        session.accept(Direction::WorkerToGateway, frame.clone())?;
        match frame.kind() {
            FrameKind::BuildStarted | FrameKind::LogChunk => {}
            FrameKind::BuildResult => {
                let result: telchar::nomad::protocol::BuildResultMetadata = decode_metadata(frame.metadata(), METADATA)?;
                if result.outcome == telchar::nomad::protocol::BuildOutcome::Failed {
                    return Err(io::Error::other("worker reported build failure"));
                }
                return Ok(());
            }
            _ => return Err(io::Error::other("output collection not configured")),
        }
    }
}

struct Authentication {
    execution: String,
    instance: String,
    credential: String,
    consumed: bool,
}

impl Authentication {
    fn new(execution: &str, instance: &str, credential: &str) -> Self {
        Self {
            execution: execution.to_owned(),
            instance: instance.to_owned(),
            credential: format!("Bearer {credential}"),
            consumed: false,
        }
    }

    fn accept(&mut self, request: &tungstenite::http::Request<()>) -> bool {
        let matches = |name: &str, expected: &str| {
            request.headers().get(name).is_some_and(|value| value.as_bytes() == expected.as_bytes())
        };
        if self.consumed || request.uri().path() != "/fixture"
            || !matches("x-telchar-execution", &self.execution)
            || !matches("x-telchar-microvm", &self.instance)
            || !matches("authorization", &self.credential)
        {
            return false;
        }
        self.consumed = true;
        true
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 4 {
        return Err("expected config.json certificate.pem key.pem listen-address".into());
    }
    let configuration: serde_json::Value = serde_json::from_reader(std::fs::File::open(&arguments[0])?)?;
    let required = |name: &str| configuration[name].as_str().ok_or("missing execution field");
    let mut authentication = Authentication::new(
        required("execution")?, required("microvmId")?, required("credential")?,
    );
    let certificates = rustls::pki_types::CertificateDer::pem_file_iter(&arguments[1])?
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_file(&arguments[2])?;
    let tls = std::sync::Arc::new(rustls::ServerConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth().with_single_cert(certificates, key)?);
    let listener = std::net::TcpListener::bind(&arguments[3])?;
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    while std::time::Instant::now() < deadline {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
        let stream = rustls::StreamOwned::new(rustls::ServerConnection::new(tls.clone())?, stream);
        let accepted = tungstenite::accept_hdr(stream, |request: &tungstenite::handshake::server::Request, response| {
            if authentication.accept(request) {
                Ok(response)
            } else {
                Err(tungstenite::http::Response::builder().status(403).body(None).expect("valid rejection"))
            }
        });
        match accepted {
            Ok(mut socket) => {
                socket.close(None)?;
                println!("execution authentication accepted");
                return Ok(());
            }
            Err(_) => eprintln!("execution authentication rejected"),
        }
    }
    Err("receiver lifetime exhausted".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::http::Request;

    #[test]
    fn exchange_streams_admitted_input_and_reports_build_failure() {
        use telchar::nomad::protocol::*;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let directory = std::env::temp_dir().join(format!("telchar-exchange-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let manifest = manifest();
        std::fs::write(directory.join("0.nar"), vec![9; 42]).unwrap();
        let server_directory = directory.clone();
        let input = manifest.paths[0].path.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            exchange(&mut socket, manifest, &server_directory, "unix:///nonexistent/socket")
                .expect_err("build failure propagated")
        });
        let (mut socket, _) = tungstenite::connect(format!("ws://{address}/fixture")).unwrap();
        assert_eq!(receive(&mut socket).unwrap().kind(), FrameKind::InputManifest);
        send(&mut socket, &Frame::new(FrameKind::ValidPaths, encode_metadata(&PathSet { paths: vec![] }, METADATA).unwrap(), vec![])).unwrap();
        send(&mut socket, &Frame::new(FrameKind::InputRequest, encode_metadata(&PathSet { paths: vec![input] }, METADATA).unwrap(), vec![])).unwrap();
        let received = receive(&mut socket).unwrap();
        assert_eq!(received.kind(), FrameKind::InputNar);
        assert_eq!(received.payload(), vec![9; 42]);
        send(&mut socket, &Frame::new(FrameKind::BuildStarted, encode_metadata(&BuildStarted { derivation_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-build.drv".to_owned() }, METADATA).unwrap(), vec![])).unwrap();
        send(&mut socket, &Frame::new(FrameKind::BuildResult, encode_metadata(&BuildResultMetadata { outcome: BuildOutcome::Failed, diagnostic: Some("fixture failure".to_owned()) }, METADATA).unwrap(), vec![])).unwrap();
        let error = server.join().unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(error.to_string(), "worker reported build failure");
    }

    #[test]
    fn streams_input_in_bounded_ordered_frames() {
        use telchar::nomad::protocol::*;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let entry = PathManifestEntry {
            path: "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input".to_owned(),
            nar_hash: "0".repeat(64), nar_size: (CHUNK + 7) as u64,
            references: vec![], deriver: None, content_address: None,
        };
        let expected = entry.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            stream_input(&mut socket, &entry, &mut io::Cursor::new(vec![9; CHUNK + 7]), |_| Ok(())).unwrap();
        });
        let (mut socket, _) = tungstenite::connect(format!("ws://{address}/fixture")).unwrap();
        for (offset, length, final_chunk) in [(0, CHUNK, false), (CHUNK, 7, true)] {
            let frame = receive(&mut socket).unwrap();
            assert_eq!(frame.kind(), FrameKind::InputNar);
            let metadata: NarMetadata = decode_metadata(frame.metadata(), METADATA).unwrap();
            assert_eq!(metadata.path, expected.path);
            assert_eq!(metadata.offset, offset as u64);
            assert_eq!(metadata.final_chunk, final_chunk);
            assert_eq!(frame.payload(), vec![9; length]);
        }
        server.join().unwrap();
    }

    fn manifest() -> InputManifest {
        use telchar::nomad::protocol::*;
        let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-build.drv";
        let input = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input";
        let output = "/nix/store/cccccccccccccccccccccccccccccccc-output";
        InputManifest {
            derivation_path: derivation.to_owned(),
            build: BuildSpecification {
                derivation_path: derivation.as_bytes().to_vec(),
                outputs: vec![NamedOutput { name: b"out".to_vec(), path: output.as_bytes().to_vec(), hash_algorithm: vec![], hash: vec![] }],
                input_sources: vec![input.as_bytes().to_vec()],
                system: "x86_64-linux".to_owned(), required_system_features: vec![],
                builder: b"/bin/sh".to_vec(), arguments: vec![b"-e".to_vec()],
                environment: vec![(b"out".to_vec(), output.as_bytes().to_vec())],
            },
            paths: vec![PathManifestEntry { path: input.to_owned(), nar_hash: "0".repeat(64), nar_size: 42, references: vec![], deriver: None, content_address: None }],
            outputs: vec![output.to_owned()],
        }
    }

    #[test]
    fn exchange_sends_manifest_and_rejects_foreign_input_request() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let manifest = manifest();
        let expected = manifest.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            exchange(&mut socket, manifest, std::path::Path::new("/nonexistent-fixture"), "unix:///nonexistent/socket")
                .expect_err("foreign input request rejected")
        });
        let (mut socket, _) = tungstenite::connect(format!("ws://{address}/fixture")).unwrap();
        let frame = receive(&mut socket).unwrap();
        assert_eq!(frame.kind(), FrameKind::InputManifest);
        let received: InputManifest = decode_metadata(frame.metadata(), METADATA).unwrap();
        assert_eq!(received, expected);
        send(&mut socket, &Frame::new(FrameKind::ValidPaths, encode_metadata(&PathSet { paths: vec![] }, METADATA).unwrap(), vec![])).unwrap();
        send(&mut socket, &Frame::new(FrameKind::InputRequest, encode_metadata(&PathSet { paths: vec!["/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-foreign".to_owned()] }, METADATA).unwrap(), vec![])).unwrap();
        assert_eq!(server.join().unwrap().kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn authentication_binds_instance_execution_and_single_use_credential() {
        let mut authentication = Authentication::new("execution", "instance", "secret");
        let request = |execution: &str, instance: &str, credential: &str| {
            Request::builder()
                .uri("/fixture")
                .header("x-telchar-execution", execution)
                .header("x-telchar-microvm", instance)
                .header("authorization", format!("Bearer {credential}"))
                .body(())
                .unwrap()
        };
        for rejected in [
            request("foreign", "instance", "secret"),
            request("execution", "foreign", "secret"),
            request("execution", "instance", "wrong"),
        ] {
            assert!(!authentication.accept(&rejected));
        }
        let accepted = request("execution", "instance", "secret");
        assert!(authentication.accept(&accepted));
        assert!(!authentication.accept(&accepted));
    }
}
