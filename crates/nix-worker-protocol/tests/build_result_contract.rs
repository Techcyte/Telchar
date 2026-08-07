use nix_worker_protocol::{write_build_derivation_success_response, WorkerVersion};

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
