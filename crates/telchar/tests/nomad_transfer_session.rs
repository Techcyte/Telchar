use telchar::nomad_transfer_protocol::{
    encode_metadata, BuildOutcome, BuildResultMetadata, BuildSpecification, Direction, Frame,
    FrameKind, InputManifest, LogChunk, NamedOutput, NarMetadata, OutputReceipt, PathManifestEntry,
    PathSet, TransferSession,
};

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn path(hash: char, name: &str) -> String {
    format!("/nix/store/{}-{name}", hash.to_string().repeat(32))
}

fn manifest() -> InputManifest {
    let derivation_path = path('a', "build.drv");
    let output = path('c', "output");
    InputManifest {
        derivation_path: derivation_path.clone(),
        build: BuildSpecification {
            derivation_path: derivation_path.into_bytes(),
            outputs: vec![NamedOutput {
                name: b"out".to_vec(),
                path: output.clone().into_bytes(),
            }],
            input_sources: vec![path('b', "input").into_bytes()],
            system: "x86_64-linux".to_owned(),
            required_system_features: vec![],
            builder: b"/bin/sh".to_vec(),
            arguments: vec![b"-e".to_vec()],
            environment: vec![
                (b"system".to_vec(), b"x86_64-linux".to_vec()),
                (b"builder".to_vec(), b"/bin/sh".to_vec()),
                (b"name".to_vec(), b"build".to_vec()),
                (b"out".to_vec(), output.clone().into_bytes()),
            ],
        },
        paths: vec![PathManifestEntry {
            path: path('b', "input"),
            nar_hash: HASH.to_owned(),
            nar_size: 10,
            references: vec![],
            deriver: None,
        }],
        outputs: vec![output],
    }
}

fn frame<T: serde::Serialize>(kind: FrameKind, metadata: &T, payload: Vec<u8>) -> Frame {
    Frame::new(
        kind,
        encode_metadata(metadata, 4096).expect("metadata encodes"),
        payload,
    )
}

#[test]
fn rejects_manifest_when_exact_build_specification_disagrees() {
    let mut manifest = manifest();
    manifest.build.outputs[0].path = path('d', "foreign").into_bytes();

    assert!(TransferSession::new(manifest, 8, 32, 32, 32, 32, 16, 4096).is_err());
}

#[test]
fn validates_complete_selective_transfer_sequence() {
    let manifest = manifest();
    let input = manifest.paths[0].path.clone();
    let output = manifest.outputs[0].clone();
    let mut session =
        TransferSession::new(manifest, 8, 32, 32, 32, 32, 16, 4096).expect("session creates");

    session
        .accept(
            Direction::WorkerToGateway,
            frame(FrameKind::ValidPaths, &PathSet { paths: vec![] }, vec![]),
        )
        .expect("valid paths accepts");
    session
        .accept(
            Direction::WorkerToGateway,
            frame(
                FrameKind::InputRequest,
                &PathSet {
                    paths: vec![input.clone()],
                },
                vec![],
            ),
        )
        .expect("input request accepts");
    session
        .accept(
            Direction::GatewayToWorker,
            frame(
                FrameKind::InputNar,
                &NarMetadata {
                    path: input,
                    nar_hash: HASH.to_owned(),
                    nar_size: 10,
                },
                vec![0; 10],
            ),
        )
        .expect("input NAR accepts");
    session
        .accept(
            Direction::WorkerToGateway,
            frame(FrameKind::BuildStarted, &json_build_started(), vec![]),
        )
        .expect("build start accepts");
    session
        .accept(
            Direction::WorkerToGateway,
            frame(
                FrameKind::LogChunk,
                &LogChunk { sequence: 0 },
                b"log".to_vec(),
            ),
        )
        .expect("log accepts");
    session
        .accept(
            Direction::WorkerToGateway,
            frame(
                FrameKind::OutputMetadata,
                &NarMetadata {
                    path: output.clone(),
                    nar_hash: HASH.to_owned(),
                    nar_size: 12,
                },
                vec![],
            ),
        )
        .expect("output metadata accepts");
    session
        .accept(
            Direction::WorkerToGateway,
            frame(
                FrameKind::OutputNar,
                &PathSet {
                    paths: vec![output.clone()],
                },
                vec![0; 12],
            ),
        )
        .expect("output NAR accepts");
    session
        .accept(
            Direction::GatewayToWorker,
            frame(
                FrameKind::OutputReceipt,
                &OutputReceipt {
                    path: output,
                    accepted: true,
                },
                vec![],
            ),
        )
        .expect("receipt accepts");
    session
        .accept(
            Direction::WorkerToGateway,
            frame(
                FrameKind::BuildResult,
                &BuildResultMetadata {
                    outcome: BuildOutcome::Built,
                    diagnostic: None,
                },
                vec![],
            ),
        )
        .expect("terminal success accepts");
    assert!(session.is_complete());
}

fn json_build_started() -> serde_json::Value {
    serde_json::json!({"derivation_path": path('a', "build.drv")})
}

#[test]
fn rejects_log_sequence_gaps_payload_misuse_and_early_success() {
    let manifest = manifest();
    let mut session =
        TransferSession::new(manifest, 8, 32, 32, 32, 32, 4, 4096).expect("session creates");
    session
        .accept(
            Direction::WorkerToGateway,
            frame(
                FrameKind::ValidPaths,
                &PathSet {
                    paths: vec![path('b', "input")],
                },
                vec![],
            ),
        )
        .expect("valid paths accepts");
    session
        .accept(
            Direction::WorkerToGateway,
            frame(FrameKind::InputRequest, &PathSet { paths: vec![] }, vec![]),
        )
        .expect("empty request accepts");
    session
        .accept(
            Direction::WorkerToGateway,
            frame(FrameKind::BuildStarted, &json_build_started(), vec![]),
        )
        .expect("build starts");
    assert!(session
        .accept(
            Direction::WorkerToGateway,
            frame(
                FrameKind::LogChunk,
                &LogChunk { sequence: 1 },
                b"gap".to_vec()
            ),
        )
        .is_err());
    assert!(session
        .accept(
            Direction::WorkerToGateway,
            frame(
                FrameKind::LogChunk,
                &LogChunk { sequence: 0 },
                b"large".to_vec()
            ),
        )
        .is_err());
    assert!(session
        .accept(
            Direction::WorkerToGateway,
            frame(
                FrameKind::BuildResult,
                &BuildResultMetadata {
                    outcome: BuildOutcome::Built,
                    diagnostic: None,
                },
                vec![],
            ),
        )
        .is_err());
}
