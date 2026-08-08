use std::io;
use std::time::Duration;

use nix_worker_protocol::{
    LATEST_WORKER_VERSION, MAXIMUM_WORKER_STORE_PATH_BYTES, ProtocolSessionLimits, WorkerOperation,
    WorkerReader, write_query_path_info_response, write_worker_byte_string, write_worker_integer,
};

const PATH: &[u8] = b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-telchar-output";

#[test]
fn decodes_bounded_query_path_info_request() {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 26);
    write_worker_byte_string(&mut wire, PATH);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );

    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::QueryPathInfo
    );
    let request = reader.complete_store_path_request().unwrap();

    assert_eq!(request.path(), PATH);
}

#[test]
fn decodes_bounded_nar_from_path_request() {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 38);
    write_worker_byte_string(&mut wire, PATH);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );

    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::NarFromPath
    );
    let request = reader.complete_store_path_request().unwrap();

    assert_eq!(request.path(), PATH);
}

#[test]
fn rejects_oversized_store_path_before_allocation() {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 26);
    write_worker_integer(&mut wire, MAXIMUM_WORKER_STORE_PATH_BYTES as u64 + 1);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );
    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::QueryPathInfo
    );

    let error = reader.complete_store_path_request().unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(reader.retained_metadata_bytes(), 0);
}

#[test]
fn encodes_present_query_path_info_response() {
    let mut response = Vec::new();
    write_query_path_info_response(
        &mut response,
        LATEST_WORKER_VERSION,
        Some(nix_worker_protocol::PathInfoResponse {
            deriver: None,
            nar_hash_hex: "6c2be2f12a168605ebcba3782286c38eaf3f5d787b7a8bb24a540d2267ff68e1",
            references: &[],
            registration_time: 0,
            nar_size: 136,
            ultimate: false,
            signatures: &[],
            content_address: None,
        }),
    )
    .unwrap();
    let mut expected = Vec::new();
    write_worker_integer(&mut expected, 1);
    write_worker_byte_string(&mut expected, b"");
    write_worker_byte_string(
        &mut expected,
        b"6c2be2f12a168605ebcba3782286c38eaf3f5d787b7a8bb24a540d2267ff68e1",
    );
    write_worker_integer(&mut expected, 0);
    write_worker_integer(&mut expected, 0);
    write_worker_integer(&mut expected, 136);
    write_worker_integer(&mut expected, 0);
    write_worker_integer(&mut expected, 0);
    write_worker_byte_string(&mut expected, b"");

    assert_eq!(response, expected);
}

#[test]
fn encodes_missing_query_path_info_response() {
    let mut response = Vec::new();
    write_query_path_info_response(&mut response, LATEST_WORKER_VERSION, None).unwrap();

    assert_eq!(response, 0_u64.to_le_bytes());
}
