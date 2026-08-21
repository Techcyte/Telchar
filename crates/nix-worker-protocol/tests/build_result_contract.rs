//! Tests build result contract contracts and failure boundaries, including writes latest empty success result matching pinned field order.

use nix_worker_protocol::{
    write_build_derivation_success_response, write_build_paths_with_results_success_response,
    write_worker_byte_string, write_worker_integer, ProtocolSessionLimits, WorkerOperation,
    WorkerReader, WorkerVersion,
};

#[test]
fn decodes_bounded_build_paths_with_results_request() {
    let target = b"/nix/store/00000000000000000000000000000000-contract.drv!*";
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 46);
    write_worker_integer(&mut wire, 1);
    write_worker_byte_string(&mut wire, target);
    write_worker_integer(&mut wire, 0);
    let mut reader = WorkerReader::new(wire.as_slice(), ProtocolSessionLimits::DEFAULT);

    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::BuildPathsWithResults
    );
    let request = reader.complete_build_paths_with_results().unwrap();
    assert_eq!(request.targets(), &[target.to_vec()]);
    assert_eq!(request.build_mode(), 0);
}

#[test]
fn writes_keyed_build_paths_success_result() {
    let mut output = Vec::new();
    let target = b"/nix/store/00000000000000000000000000000000-contract.drv!*";

    write_build_paths_with_results_success_response(
        &mut output,
        WorkerVersion::new(1, 38),
        [(target.as_slice(), false)],
    )
    .expect("keyed success response writes");

    let mut input = output.as_slice();
    assert_eq!(read_integer(&mut input), nix_worker_protocol::STDERR_LAST);
    assert_eq!(read_integer(&mut input), 1, "one keyed result");
    assert_eq!(read_string(&mut input), target);
    assert_eq!(read_integer(&mut input), 0, "Built status");
    assert_eq!(read_string(&mut input), b"");
    for _ in 0..6 {
        assert_eq!(read_integer(&mut input), 0);
    }
    assert_eq!(read_integer(&mut input), 0, "no CA realisations");
    assert!(input.is_empty());
}

#[test]
fn writes_latest_empty_success_result_matching_pinned_field_order() {
    let mut output = Vec::new();

    write_build_derivation_success_response(&mut output, WorkerVersion::new(1, 38), false)
        .expect("success response writes");

    let mut input = output.as_slice();
    assert_eq!(read_integer(&mut input), nix_worker_protocol::STDERR_LAST);
    assert_eq!(read_integer(&mut input), 0, "Built status");
    assert_eq!(read_string(&mut input), b"");
    assert_eq!(read_integer(&mut input), 0, "times built");
    assert_eq!(read_integer(&mut input), 0, "not nondeterministic");
    assert_eq!(read_integer(&mut input), 0, "start time");
    assert_eq!(read_integer(&mut input), 0, "stop time");
    assert_eq!(read_integer(&mut input), 0, "no user CPU duration");
    assert_eq!(read_integer(&mut input), 0, "no system CPU duration");
    assert_eq!(read_integer(&mut input), 0, "no CA realisations");
    assert!(input.is_empty());
}

#[test]
fn writes_already_valid_status_and_respects_version_gates() {
    let mut output = Vec::new();

    write_build_derivation_success_response(&mut output, WorkerVersion::new(1, 27), true)
        .expect("success response writes");

    let mut input = output.as_slice();
    assert_eq!(read_integer(&mut input), nix_worker_protocol::STDERR_LAST);
    assert_eq!(read_integer(&mut input), 2, "AlreadyValid status");
    assert_eq!(read_string(&mut input), b"");
    assert!(input.is_empty());
}

fn read_integer(input: &mut &[u8]) -> u64 {
    let (value, rest) = input.split_at(8);
    *input = rest;
    u64::from_le_bytes(value.try_into().expect("integer width"))
}

fn read_string<'a>(input: &mut &'a [u8]) -> &'a [u8] {
    let length = read_integer(input) as usize;
    let (value, rest) = input.split_at(length);
    let padding = (8 - length % 8) % 8;
    *input = &rest[padding..];
    value
}
