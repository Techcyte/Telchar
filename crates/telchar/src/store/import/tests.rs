//! Regression coverage for metadata crossing the worker-protocol import boundary.

use std::io;
use std::time::Duration;

use nix_worker_protocol::{
    LATEST_WORKER_VERSION, ProtocolSessionLimits, WorkerOperation, WorkerReader,
    write_worker_byte_string, write_worker_integer,
};

use super::declared_path_info;

fn imported_content_address(content_address: &[u8]) -> io::Result<Option<String>> {
    let mut logical = Vec::new();
    write_worker_integer(&mut logical, 1);
    write_worker_byte_string(
        &mut logical,
        b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-source",
    );
    write_worker_byte_string(&mut logical, b"");
    write_worker_byte_string(&mut logical, b"0".repeat(64).as_slice());
    for _ in 0..5 {
        write_worker_integer(&mut logical, 0);
    }
    write_worker_byte_string(&mut logical, content_address);

    let mut wire = Vec::new();
    for value in [44, 0, 1, logical.len() as u64] {
        write_worker_integer(&mut wire, value);
    }
    wire.extend_from_slice(&logical);
    write_worker_integer(&mut wire, 0);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024, Duration::from_secs(1)),
    );
    assert_eq!(
        reader.read_operation()?,
        WorkerOperation::AddMultipleToStore
    );
    let mut imported = None;
    reader
        .complete_add_multiple_to_store(LATEST_WORKER_VERSION, |info, _| {
            imported = Some(declared_path_info(info)?.content_address);
            Ok(())
        })
        .map_err(|error| io::Error::new(error.kind(), error.to_string()))?;
    Ok(imported.expect("one path imported"))
}

#[test]
fn preserves_fixed_output_content_address_from_worker_protocol() {
    let address = "fixed:r:sha256:0000000000000000000000000000000000000000000000000000";
    assert_eq!(
        imported_content_address(address.as_bytes()).unwrap(),
        Some(address.to_owned())
    );
}

#[test]
fn preserves_absent_content_address_for_input_addressed_paths() {
    assert_eq!(imported_content_address(b"").unwrap(), None);
}

#[test]
fn rejects_non_utf8_content_address() {
    assert_eq!(
        imported_content_address(b"\xff").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
