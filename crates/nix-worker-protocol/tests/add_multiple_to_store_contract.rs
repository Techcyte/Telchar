//! Tests add multiple to store contract contracts and failure boundaries, including decodes empty add multiple to store request.

use std::io;
use std::time::Duration;

use nix_worker_protocol::{
    write_worker_byte_string, write_worker_integer, ProtocolSessionLimits, WorkerOperation,
    WorkerReader, WorkerVersion, LATEST_WORKER_VERSION,
};

#[test]
fn decodes_empty_add_multiple_to_store_request() {
    let mut wire = request_prefix(0, 1);
    append_frame(&mut wire, &0_u64.to_le_bytes());
    write_worker_integer(&mut wire, 0);
    let mut reader = reader(&wire);

    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::AddMultipleToStore
    );
    let request = reader
        .complete_empty_add_multiple_to_store(LATEST_WORKER_VERSION)
        .expect("empty batch decodes");

    assert!(!request.repair());
    assert!(request.dont_check_signatures());
}

#[test]
fn decodes_inner_count_split_across_protocol_frames() {
    let count = 0_u64.to_le_bytes();
    let mut wire = request_prefix(0, 0);
    append_frame(&mut wire, &count[..3]);
    append_frame(&mut wire, &count[3..]);
    write_worker_integer(&mut wire, 0);
    let mut reader = reader(&wire);

    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::AddMultipleToStore
    );
    let request = reader
        .complete_empty_add_multiple_to_store(LATEST_WORKER_VERSION)
        .expect("split count decodes");

    assert!(!request.dont_check_signatures());
}

#[test]
fn streams_one_declared_path_body_without_retaining_it() {
    let path = b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-source";
    let nar = b"streamed-nar";
    let mut logical = Vec::new();
    write_worker_integer(&mut logical, 1);
    write_worker_byte_string(&mut logical, path);
    write_worker_byte_string(&mut logical, b"");
    write_worker_byte_string(
        &mut logical,
        b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    );
    write_worker_integer(&mut logical, 0);
    write_worker_integer(&mut logical, 123);
    write_worker_integer(&mut logical, nar.len() as u64);
    write_worker_integer(&mut logical, 0);
    write_worker_integer(&mut logical, 0);
    write_worker_byte_string(&mut logical, b"");
    logical.extend_from_slice(nar);

    let mut wire = request_prefix(0, 1);
    append_frame(&mut wire, &logical[..17]);
    append_frame(&mut wire, &logical[17..]);
    write_worker_integer(&mut wire, 0);
    let mut reader = reader(&wire);
    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::AddMultipleToStore
    );

    let mut body = Vec::new();
    let request = reader
        .complete_add_multiple_to_store(LATEST_WORKER_VERSION, |info, source| {
            assert_eq!(info.path(), path);
            assert_eq!(info.nar_size(), nar.len() as u64);
            assert!(info.references().is_empty());
            assert!(!info.ultimate());
            std::io::Read::read_to_end(source, &mut body)?;
            Ok(())
        })
        .expect("nonempty batch decodes");

    assert_eq!(request.object_count(), 1);
    assert!(request.dont_check_signatures());
    assert_eq!(body, nar);
}

#[test]
fn accepts_realistic_input_closure_object_count() {
    let mut logical = Vec::new();
    write_worker_integer(&mut logical, 2_338);
    let mut wire = request_prefix(0, 1);
    append_frame(&mut wire, &logical);
    write_worker_integer(&mut wire, 0);
    let mut reader = reader(&wire);
    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::AddMultipleToStore
    );

    let error = reader
        .complete_add_multiple_to_store(LATEST_WORKER_VERSION, |_, _| Ok(()))
        .expect_err("truncated first object must fail after accepting batch count");

    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn rejects_repair_invalid_booleans_trailing_bytes_and_missing_terminator() {
    for (repair, dont_check) in [(1, 0), (2, 0), (0, 2)] {
        let mut wire = request_prefix(repair, dont_check);
        append_frame(&mut wire, &0_u64.to_le_bytes());
        write_worker_integer(&mut wire, 0);
        let mut reader = reader(&wire);
        assert_eq!(
            reader.read_operation().unwrap(),
            WorkerOperation::AddMultipleToStore
        );
        assert!(
            reader
                .complete_empty_add_multiple_to_store(LATEST_WORKER_VERSION)
                .is_err(),
            "flags {repair}/{dont_check} accepted"
        );
    }

    let mut trailing = request_prefix(0, 1);
    append_frame(&mut trailing, &[0; 9]);
    write_worker_integer(&mut trailing, 0);
    let mut trailing_reader = reader(&trailing);
    assert_eq!(
        trailing_reader.read_operation().unwrap(),
        WorkerOperation::AddMultipleToStore
    );
    assert_eq!(
        trailing_reader
            .complete_empty_add_multiple_to_store(LATEST_WORKER_VERSION)
            .expect_err("trailing logical bytes must fail")
            .kind(),
        io::ErrorKind::InvalidData
    );

    let mut missing_terminator = request_prefix(0, 1);
    append_frame(&mut missing_terminator, &0_u64.to_le_bytes());
    let mut terminator_reader = reader(&missing_terminator);
    assert_eq!(
        terminator_reader.read_operation().unwrap(),
        WorkerOperation::AddMultipleToStore
    );
    assert_eq!(
        terminator_reader
            .complete_empty_add_multiple_to_store(LATEST_WORKER_VERSION)
            .expect_err("framed stream terminator is required")
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn rejects_operation_before_worker_protocol_1_32() {
    let mut wire = request_prefix(0, 1);
    append_frame(&mut wire, &0_u64.to_le_bytes());
    write_worker_integer(&mut wire, 0);
    let mut reader = reader(&wire);
    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::AddMultipleToStore
    );

    let error = reader
        .complete_empty_add_multiple_to_store(WorkerVersion::new(1, 31))
        .expect_err("operation requires worker protocol 1.32");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

fn request_prefix(repair: u64, dont_check_signatures: u64) -> Vec<u8> {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 44);
    write_worker_integer(&mut wire, repair);
    write_worker_integer(&mut wire, dont_check_signatures);
    wire
}

fn append_frame(wire: &mut Vec<u8>, body: &[u8]) {
    write_worker_integer(wire, body.len() as u64);
    wire.extend_from_slice(body);
}

fn reader(wire: &[u8]) -> WorkerReader<&[u8]> {
    WorkerReader::new(
        wire,
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    )
}
