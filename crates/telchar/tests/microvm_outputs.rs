use sha2::{Digest, Sha256};
use telchar::microvm::outputs::Outputs;
use telchar::nomad::protocol::{BuildOutcome, BuildResultMetadata, NarMetadata, PathManifestEntry};
use telchar::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

#[test]
fn disk_reserve_rejects_output_before_staging() {
    const CHILD: &str = "TELCHAR_TEST_OUTPUT_DISK_RESERVE";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "disk_reserve_rejects_output_before_staging"])
            .env(CHILD, "1")
            .env("TELCHAR_GATEWAY_DISK_RESERVE_BYTES", u64::MAX.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        return;
    }
    let staging = tempfile::tempdir().unwrap();
    let path = "/nix/store/cccccccccccccccccccccccccccccccc-microvm-output";
    let mut outputs = Outputs::new(
        vec![path.into()],
        GatewayStoreEndpoint::parse("unix:///unused-nix.sock").unwrap(),
        staging.path().into(),
        1024,
        2048,
    )
    .unwrap();
    let error = outputs
        .declare(PathManifestEntry {
            path: path.into(),
            nar_hash: "00".repeat(32),
            nar_size: 128,
            references: vec![],
            deriver: None,
            content_address: None,
        })
        .expect_err("disk reserve must reject before accepting output bytes");
    assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
    assert!(
        outputs
            .finish(&BuildResultMetadata {
                outcome: BuildOutcome::Built,
                diagnostic: None
            })
            .is_err()
    );
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
}

#[test]
fn receipts_require_authoritative_registration() {
    let Ok(uri) = std::env::var("MICROVM_TEST_STORE_URI") else {
        let output = std::process::Command::new("bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/microvm/canonical-store.sh"
            ))
            .arg(std::env::current_exe().unwrap())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let endpoint = GatewayStoreEndpoint::parse(&uri).unwrap();
    let staging = tempfile::tempdir().unwrap();
    let mut nar = Vec::new();
    for value in [
        b"nix-archive-1".as_slice(),
        b"(",
        b"type",
        b"regular",
        b"contents",
        b"microvm-output",
        b")",
    ] {
        nar.extend_from_slice(&(value.len() as u64).to_le_bytes());
        nar.extend_from_slice(value);
        nar.resize(nar.len().next_multiple_of(8), 0);
    }
    let path = "/nix/store/cccccccccccccccccccccccccccccccc-microvm-output";
    let metadata = PathManifestEntry {
        path: path.into(),
        nar_hash: Sha256::digest(&nar)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        nar_size: nar.len() as u64,
        references: vec![],
        deriver: None,
        content_address: None,
    };
    let result = BuildResultMetadata {
        outcome: BuildOutcome::Built,
        diagnostic: None,
    };
    let mut store = GatewayStoreConnection::connect(&endpoint).unwrap();
    assert!(!store.is_valid_path(path.as_bytes()).unwrap());
    for corrupt in [true, false] {
        let mut outputs = Outputs::new(
            vec![path.into()],
            endpoint.clone(),
            staging.path().into(),
            1024,
            2048,
        )
        .unwrap();
        outputs.declare(metadata.clone()).unwrap();
        assert!(outputs.finish(&result).is_err());
        let chunk = NarMetadata {
            path: path.into(),
            nar_hash: metadata.nar_hash.clone(),
            nar_size: metadata.nar_size,
            offset: 0,
            final_chunk: true,
        };
        let mut bytes = nar.clone();
        if corrupt {
            let index = bytes
                .windows(14)
                .position(|value| value == b"microvm-output")
                .unwrap();
            bytes[index] ^= 1;
        }
        let receipt = outputs.receive(&chunk, &bytes);
        if corrupt {
            assert_eq!(receipt.unwrap_err().to_string(), "NAR hash mismatch");
            assert!(!store.is_valid_path(path.as_bytes()).unwrap());
            assert!(outputs.finish(&result).is_err());
        } else {
            assert!(receipt.unwrap().unwrap().accepted);
            let info = store.query_path_info(path.as_bytes()).unwrap().unwrap();
            assert_eq!(info.nar_hash_hex(), metadata.nar_hash);
            assert_eq!(info.nar_size(), metadata.nar_size);
            outputs.finish(&result).unwrap();
        }
        assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    }
    for rejection in [
        Some("reference"),
        Some("deriver"),
        Some("content address"),
        None,
    ] {
        gateway_session(&endpoint, staging.path(), &metadata, &nar, rejection);
    }
}

fn gateway_session(
    endpoint: &GatewayStoreEndpoint,
    staging: &std::path::Path,
    input: &PathManifestEntry,
    nar: &[u8],
    rejection: Option<&str>,
) {
    use std::net::TcpListener;
    use std::time::Duration;
    use telchar::microvm::session::{Limits, Session};
    use telchar::nomad::protocol::*;
    let output = "/nix/store/dddddddddddddddddddddddddddddddd-session-output";
    let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-session.drv";
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let limits = ProtocolLimits::new(8192, 256);
    let mut output_info = input.clone();
    output_info.path = output.into();
    match rejection {
        Some("reference") => {
            output_info.references =
                vec!["/nix/store/ffffffffffffffffffffffffffffffff-unadmitted".into()]
        }
        Some("deriver") => {
            output_info.deriver =
                Some("/nix/store/ffffffffffffffffffffffffffffffff-other.drv".into())
        }
        Some("content address") => {
            output_info.content_address = Some(format!("fixed:sha256:{}", "0".repeat(52)))
        }
        _ => {}
    }
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{address}")).unwrap();
            let read = |socket: &mut tungstenite::WebSocket<_>| {
                let body = socket.read().unwrap().into_data();
                read_frame(&mut body.as_ref(), limits).unwrap()
            };
            let send = |socket: &mut tungstenite::WebSocket<_>, kind, metadata, payload| {
                let mut body = Vec::new();
                write_frame(&mut body, &Frame::new(kind, metadata, payload), limits).unwrap();
                socket
                    .send(tungstenite::Message::Binary(body.into()))
                    .unwrap();
            };
            let received = read(&mut socket);
            assert_eq!(received.kind(), FrameKind::InputManifest);
            let manifest: InputManifest = decode_metadata(received.metadata(), 8192).unwrap();
            send(
                &mut socket,
                FrameKind::ValidPaths,
                encode_metadata(&PathSet { paths: vec![] }, 8192).unwrap(),
                vec![],
            );
            send(
                &mut socket,
                FrameKind::InputRequest,
                encode_metadata(
                    &PathSet {
                        paths: vec![input.path.clone()],
                    },
                    8192,
                )
                .unwrap(),
                vec![],
            );
            let mut transferred = Vec::new();
            loop {
                let frame = read(&mut socket);
                assert_eq!(frame.kind(), FrameKind::InputNar);
                let chunk: NarMetadata = decode_metadata(frame.metadata(), 8192).unwrap();
                assert_eq!(chunk.offset, transferred.len() as u64);
                transferred.extend_from_slice(frame.payload());
                if chunk.final_chunk {
                    break;
                }
            }
            assert_eq!(transferred, nar);
            send(
                &mut socket,
                FrameKind::BuildStarted,
                encode_metadata(
                    &BuildStarted {
                        derivation_path: manifest.derivation_path,
                    },
                    8192,
                )
                .unwrap(),
                vec![],
            );
            send(
                &mut socket,
                FrameKind::LogChunk,
                encode_metadata(&LogChunk { sequence: 0 }, 8192).unwrap(),
                b"building\n".to_vec(),
            );
            send(
                &mut socket,
                FrameKind::OutputMetadata,
                encode_metadata(&output_info, 8192).unwrap(),
                vec![],
            );
            if rejection.is_some() {
                assert!(
                    socket.read().is_err(),
                    "unadmitted metadata must close session before NAR transfer"
                );
                return;
            }
            send(
                &mut socket,
                FrameKind::OutputNar,
                encode_metadata(
                    &NarMetadata {
                        path: output.into(),
                        nar_hash: output_info.nar_hash,
                        nar_size: nar.len() as u64,
                        offset: 0,
                        final_chunk: true,
                    },
                    8192,
                )
                .unwrap(),
                nar.to_vec(),
            );
            let receipt = read(&mut socket);
            assert_eq!(receipt.kind(), FrameKind::OutputReceipt);
            let receipt: OutputReceipt = decode_metadata(receipt.metadata(), 8192).unwrap();
            assert!(receipt.accepted);
            assert_eq!(receipt.path, output);
            send(
                &mut socket,
                FrameKind::BuildResult,
                encode_metadata(
                    &BuildResultMetadata {
                        outcome: BuildOutcome::Built,
                        diagnostic: None,
                    },
                    8192,
                )
                .unwrap(),
                vec![],
            );
        });
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let socket = tungstenite::accept(stream).unwrap();
        let mut logs = Vec::new();
        let derivation = format!(
            r#"Derive([("out","{output}","","")],[],["{}"],"aarch64-linux","/bin/sh",[],[("out","{output}"),("system","aarch64-linux"),("builder","/bin/sh"),("name","session")])"#,
            input.path
        );
        let request = telchar::build::BuildRequest::from_stored_derivation(
            drv.as_bytes(),
            derivation.as_bytes(),
            &[telchar::backend::BackendTarget::new(
                "fixture",
                telchar::backend::BackendKind::Local,
                "aarch64-linux",
                [] as [&str; 0],
            )
            .unwrap()],
        )
        .unwrap();
        let result = Session::new(
            &request,
            endpoint.clone(),
            staging.into(),
            Limits {
                maximum_paths: 10,
                maximum_nar_bytes: 1024,
                maximum_total_bytes: 2048,
                metadata_bytes: 8192,
                chunk_bytes: 256,
            },
        )
        .unwrap()
        .run(socket, || Ok(()), |bytes| logs.extend_from_slice(bytes));
        if let Some(field) = rejection {
            assert_eq!(
                result.unwrap_err().to_string(),
                format!("MicroVM output {field} is not admitted")
            );
        } else {
            assert_eq!(result.unwrap().outcome, BuildOutcome::Built);
        }
        assert_eq!(logs, b"building\n");
        worker.join().unwrap();
    });
    let mut store = GatewayStoreConnection::connect(endpoint).unwrap();
    assert_eq!(
        store.is_valid_path(output.as_bytes()).unwrap(),
        rejection.is_none()
    );
}
