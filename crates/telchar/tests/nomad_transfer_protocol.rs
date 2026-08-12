use std::io::Cursor;

use telchar::nomad_transfer_protocol::{
    read_frame, write_frame, Frame, FrameKind, ProtocolLimits, PROTOCOL_VERSION,
};

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
