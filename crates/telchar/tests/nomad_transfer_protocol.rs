use std::io::Cursor;

use serde::{Deserialize, Serialize};
use telchar::nomad_transfer_protocol::{
    decode_metadata, encode_metadata, read_frame, write_frame, Direction, Frame, FrameKind,
    ProtocolLimits, ProtocolSession, PROTOCOL_VERSION,
};

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct AuthenticationMetadata {
    backend: String,
    allocation_id: String,
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
    let expected = AuthenticationMetadata {
        backend: "nomad-primary".to_owned(),
        allocation_id: "allocation-1".to_owned(),
    };
    let encoded = encode_metadata(&expected, 1024).expect("metadata encodes");
    assert_eq!(
        decode_metadata::<AuthenticationMetadata>(&encoded, 1024).expect("metadata decodes"),
        expected
    );
    assert_eq!(
        encode_metadata(&expected, 4)
            .expect_err("oversized encoded metadata rejects")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert_eq!(
        decode_metadata::<AuthenticationMetadata>(&encoded, 4)
            .expect_err("oversized received metadata rejects")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
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
