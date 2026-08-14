//! Tests nomad transfer protocol contracts and failure boundaries, including build specification.

use std::io::Cursor;

use telchar::nomad::protocol::{
    decode_metadata, encode_metadata, read_frame, write_frame, Authentication, AuthenticationProof,
    BuildOutcome, BuildResultMetadata, BuildSpecification, BuildStarted, Direction, Frame,
    FrameKind, InputManifest, LogChunk, NamedOutput, NarMetadata, OutputReceipt, PathManifestEntry,
    PathSet, ProtocolLimits, ProtocolSession, PROTOCOL_VERSION,
};

fn build_specification(derivation: &str, input: &str, output: &str) -> BuildSpecification {
    BuildSpecification {
        derivation_path: derivation.as_bytes().to_vec(),
        outputs: vec![NamedOutput {
            name: b"out".to_vec(),
            path: output.as_bytes().to_vec(),
        }],
        input_sources: vec![input.as_bytes().to_vec()],
        system: "x86_64-linux".to_owned(),
        required_system_features: vec![],
        builder: b"/bin/sh".to_vec(),
        arguments: vec![b"-e".to_vec()],
        environment: vec![
            (b"system".to_vec(), b"x86_64-linux".to_vec()),
            (b"builder".to_vec(), b"/bin/sh".to_vec()),
            (b"name".to_vec(), b"build".to_vec()),
            (b"out".to_vec(), output.as_bytes().to_vec()),
        ],
    }
}

#[test]
fn round_trips_every_version_one_frame_kind() {
    let limits = ProtocolLimits::new(1024, 4096);
    let kinds = [
        FrameKind::Authenticate,
        FrameKind::InputManifest,
        FrameKind::ValidPaths,
        FrameKind::InputRequest,
        FrameKind::InputNar,
        FrameKind::BuildStarted,
        FrameKind::LogChunk,
        FrameKind::OutputMetadata,
        FrameKind::OutputNar,
        FrameKind::OutputReceipt,
        FrameKind::BuildResult,
    ];

    for kind in kinds {
        let expected = Frame::new(kind, br#"{"sequence":1}"#.to_vec(), vec![1, 2, 3]);
        let mut wire = Vec::new();
        write_frame(&mut wire, &expected, limits).expect("frame writes");
        assert_eq!(&wire[..4], b"TLNW");
        assert_eq!(u16::from_be_bytes([wire[4], wire[5]]), PROTOCOL_VERSION);
        assert_eq!(
            read_frame(&mut Cursor::new(wire), limits).expect("frame reads"),
            expected
        );
    }
}

#[test]
fn round_trips_typed_metadata_with_explicit_bounds() {
    let expected = Authentication {
        backend: "nomad-primary".to_owned(),
        namespace: "telchar".to_owned(),
        job_id: "job-1".to_owned(),
        allocation_id: "allocation-1".to_owned(),
        task: "build".to_owned(),
        shared_build_digest: "digest-1".to_owned(),
        proof: AuthenticationProof::WorkloadIdentity {
            token: "jwt".to_owned(),
        },
    };
    let encoded = encode_metadata(&expected, 1024).expect("metadata encodes");
    assert_eq!(
        decode_metadata::<Authentication>(&encoded, 1024).expect("metadata decodes"),
        expected
    );
    assert_eq!(
        encode_metadata(&expected, 4)
            .expect_err("oversized encoded metadata rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert_eq!(
        decode_metadata::<Authentication>(&encoded, 4)
            .expect_err("oversized received metadata rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn round_trips_exact_transfer_metadata_contracts() {
    let derivation_path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-build.drv".to_owned();
    let input = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input".to_owned();
    let output = "/nix/store/cccccccccccccccccccccccccccccccc-output".to_owned();
    let manifest = InputManifest {
        derivation_path: derivation_path.clone(),
        build: build_specification(&derivation_path, &input, &output),
        paths: vec![PathManifestEntry {
            path: input,
            nar_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
            nar_size: 42,
            references: vec![],
            deriver: None,
        }],
        outputs: vec![output],
    };
    let valid_paths = PathSet {
        paths: vec![manifest.paths[0].path.clone()],
    };
    let nar = NarMetadata {
        path: manifest.paths[0].path.clone(),
        nar_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
        nar_size: 42,
        offset: 0,
        final_chunk: true,
    };
    let started = BuildStarted {
        derivation_path: manifest.derivation_path.clone(),
    };
    let log = LogChunk { sequence: 1 };
    let receipt = OutputReceipt {
        path: manifest.outputs[0].clone(),
        accepted: true,
    };
    let result = BuildResultMetadata {
        outcome: BuildOutcome::Built,
        diagnostic: None,
    };

    for metadata in [
        encode_metadata(&manifest, 4096).expect("manifest encodes"),
        encode_metadata(&valid_paths, 4096).expect("valid paths encode"),
        encode_metadata(&nar, 4096).expect("NAR metadata encodes"),
        encode_metadata(&started, 4096).expect("build start encodes"),
        encode_metadata(&log, 4096).expect("log metadata encodes"),
        encode_metadata(&receipt, 4096).expect("receipt encodes"),
        encode_metadata(&result, 4096).expect("result encodes"),
    ] {
        assert!(!metadata.is_empty());
    }

    let unknown = br#"{"backend":"nomad-primary","namespace":"telchar","job_id":"job-1","allocation_id":"allocation-1","task":"build","shared_build_digest":"digest-1","proof":{"mode":"workload-identity","token":"jwt"},"unexpected":true}"#;
    assert_eq!(
        decode_metadata::<Authentication>(unknown, 4096)
            .expect_err("unknown authentication field rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn rejects_invalid_manifest_and_path_metadata() {
    let input = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input".to_owned();
    let output = "/nix/store/cccccccccccccccccccccccccccccccc-output".to_owned();
    let derivation_path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-build.drv".to_owned();
    let mut manifest = InputManifest {
        derivation_path: derivation_path.clone(),
        build: build_specification(&derivation_path, &input, &output),
        paths: vec![PathManifestEntry {
            path: input.clone(),
            nar_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
            nar_size: 42,
            references: vec![],
            deriver: None,
        }],
        outputs: vec![output.clone()],
    };
    manifest.validate(8, 1024).expect("valid manifest accepts");

    manifest.paths.push(manifest.paths[0].clone());
    assert_eq!(
        manifest
            .validate(8, 1024)
            .expect_err("duplicate manifest path rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    manifest.paths.pop();

    manifest.paths[0].nar_hash = "not-a-hash".to_owned();
    assert_eq!(
        manifest
            .validate(8, 1024)
            .expect_err("invalid NAR hash rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    manifest.paths[0].nar_hash =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned();

    manifest.paths[0].references =
        vec!["/nix/store/dddddddddddddddddddddddddddddddd-foreign".to_owned()];
    assert_eq!(
        manifest
            .validate(8, 1024)
            .expect_err("out-of-manifest reference rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );

    let requested = PathSet {
        paths: vec![input.clone()],
    };
    requested
        .validate_against(&[input], 8)
        .expect("admitted path set accepts");
    assert_eq!(
        PathSet {
            paths: vec!["/nix/store/dddddddddddddddddddddddddddddddd-foreign".to_owned()]
        }
        .validate_against(&[], 8)
        .expect_err("out-of-manifest request rejects")
        .kind(),
        std::io::ErrorKind::InvalidData
    );

    NarMetadata {
        path: output.clone(),
        nar_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
        nar_size: 1025,
        offset: 0,
        final_chunk: true,
    }
    .validate_against(&[output], 1024)
    .expect_err("oversized NAR metadata rejects");
}

#[test]
fn enforces_direction_and_transfer_phase_order() {
    let mut session = ProtocolSession::new();
    assert!(session
        .accept(Direction::WorkerToGateway, FrameKind::Authenticate)
        .is_ok());
    assert!(session
        .accept(Direction::GatewayToWorker, FrameKind::InputManifest)
        .is_ok());
    assert!(session
        .accept(Direction::WorkerToGateway, FrameKind::ValidPaths)
        .is_ok());
    assert!(session
        .accept(Direction::WorkerToGateway, FrameKind::InputRequest)
        .is_ok());
    assert!(session
        .accept(Direction::GatewayToWorker, FrameKind::InputNar)
        .is_ok());
    assert!(session
        .accept(Direction::WorkerToGateway, FrameKind::BuildStarted)
        .is_ok());
    assert!(session
        .accept(Direction::WorkerToGateway, FrameKind::LogChunk)
        .is_ok());
    assert!(session
        .accept(Direction::WorkerToGateway, FrameKind::OutputMetadata)
        .is_ok());
    assert!(session
        .accept(Direction::WorkerToGateway, FrameKind::OutputNar)
        .is_ok());
    assert!(session
        .accept(Direction::GatewayToWorker, FrameKind::OutputReceipt)
        .is_ok());
    assert!(session
        .accept(Direction::WorkerToGateway, FrameKind::BuildResult)
        .is_ok());
    assert!(session.is_complete());

    let mut unauthenticated = ProtocolSession::new();
    assert_eq!(
        unauthenticated
            .accept(Direction::WorkerToGateway, FrameKind::InputRequest)
            .expect_err("input before authentication rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );

    let mut wrong_direction = ProtocolSession::new();
    assert_eq!(
        wrong_direction
            .accept(Direction::GatewayToWorker, FrameKind::Authenticate)
            .expect_err("gateway authentication frame rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );

    let mut premature_output = ProtocolSession::new();
    premature_output
        .accept(Direction::WorkerToGateway, FrameKind::Authenticate)
        .expect("authentication accepts");
    premature_output
        .accept(Direction::GatewayToWorker, FrameKind::InputManifest)
        .expect("manifest accepts");
    assert_eq!(
        premature_output
            .accept(Direction::WorkerToGateway, FrameKind::OutputMetadata)
            .expect_err("output before build rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn rejects_oversized_metadata_and_payload_before_allocation() {
    let limits = ProtocolLimits::new(4, 8);
    let oversized_metadata = Frame::new(FrameKind::Authenticate, vec![0; 5], Vec::new());
    let oversized_payload = Frame::new(FrameKind::InputNar, Vec::new(), vec![0; 9]);

    assert_eq!(
        write_frame(&mut Vec::new(), &oversized_metadata, limits)
            .expect_err("oversized metadata rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert_eq!(
        write_frame(&mut Vec::new(), &oversized_payload, limits)
            .expect_err("oversized payload rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    let mut wire = Vec::new();
    wire.extend_from_slice(b"TLNW");
    wire.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    wire.extend_from_slice(&(FrameKind::Authenticate as u16).to_be_bytes());
    wire.extend_from_slice(&5_u32.to_be_bytes());
    wire.extend_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        read_frame(&mut Cursor::new(wire), limits)
            .expect_err("declared oversized metadata rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn rejects_unknown_versions_kinds_and_magic() {
    let limits = ProtocolLimits::new(1024, 4096);
    let frame = Frame::new(FrameKind::Authenticate, Vec::new(), Vec::new());
    let mut wire = Vec::new();
    write_frame(&mut wire, &frame, limits).expect("frame writes");

    let mut wrong_magic = wire.clone();
    wrong_magic[0] = b'X';
    assert_eq!(
        read_frame(&mut Cursor::new(wrong_magic), limits)
            .expect_err("wrong magic rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );

    let mut wrong_version = wire.clone();
    wrong_version[4..6].copy_from_slice(&(PROTOCOL_VERSION + 1).to_be_bytes());
    assert_eq!(
        read_frame(&mut Cursor::new(wrong_version), limits)
            .expect_err("unknown version rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );

    wire[6..8].copy_from_slice(&u16::MAX.to_be_bytes());
    assert_eq!(
        read_frame(&mut Cursor::new(wire), limits)
            .expect_err("unknown kind rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}
