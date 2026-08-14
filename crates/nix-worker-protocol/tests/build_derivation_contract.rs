//! Tests build derivation contract contracts and failure boundaries, including decodes gate 3 build derivation.

use std::io;
use std::time::Duration;

use nix_worker_protocol::{
    write_worker_byte_string, write_worker_integer, ProtocolSessionLimits, WorkerOperation,
    WorkerReader,
};

#[test]
fn decodes_gate_3_build_derivation() {
    let wire = gate_3_request("x86_64-linux", 0);
    let mut reader = reader(&wire);

    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::BuildDerivation
    );
    let request = reader
        .complete_build_derivation()
        .expect("Gate 3 derivation decodes");

    assert!(request
        .drv_path()
        .ends_with(b"-telchar-gate-3-contract.drv"));
    assert_eq!(request.outputs().len(), 1);
    let output = &request.outputs()[0];
    assert_eq!(output.name(), b"out");
    assert!(output.path().ends_with(b"-telchar-gate-3-contract"));
    assert_eq!(output.hash_algorithm(), b"");
    assert_eq!(output.hash(), b"");
    assert!(request.input_sources().is_empty());
    assert_eq!(request.platform(), b"x86_64-linux");
    assert_eq!(request.builder(), b"/bin/sh");
    assert_eq!(
        request.arguments(),
        [
            b"-c".as_slice(),
            b"printf telchar-remote-build > $out".as_slice()
        ]
    );
    assert_eq!(request.environment().len(), 4);
    assert_eq!(request.build_mode(), 0);
}

#[test]
fn rejects_counts_before_reading_collection_bodies() {
    for field in [
        CountField::Outputs,
        CountField::InputSources,
        CountField::Arguments,
        CountField::Environment,
    ] {
        let wire = oversized_count_request(field);
        let mut reader = reader(&wire);
        assert_eq!(
            reader.read_operation().unwrap(),
            WorkerOperation::BuildDerivation
        );
        let error = reader
            .complete_build_derivation()
            .expect_err("oversized count must fail before collection body");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{field:?}");
    }
}

#[test]
fn rejects_duplicate_outputs_inputs_and_environment_keys() {
    for wire in [
        request_with_duplicate_output(),
        request_with_duplicate_input(),
        request_with_duplicate_environment_key(),
    ] {
        let mut reader = reader(&wire);
        assert_eq!(
            reader.read_operation().unwrap(),
            WorkerOperation::BuildDerivation
        );
        assert_eq!(
            reader
                .complete_build_derivation()
                .expect_err("duplicate derivation metadata must fail")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn decodes_flat_and_recursive_fixed_output_authority() {
    for (algorithm, hash) in [
        (
            b"sha256".as_slice(),
            b"0000000000000000000000000000000000000000000000000000000000000000".as_slice(),
        ),
        (
            b"r:sha256".as_slice(),
            b"1111111111111111111111111111111111111111111111111111111111111111".as_slice(),
        ),
    ] {
        let wire = request_with_output(b"out", output_path(), algorithm, hash);
        let mut reader = reader(&wire);
        assert_eq!(
            reader.read_operation().unwrap(),
            WorkerOperation::BuildDerivation
        );
        let request = reader
            .complete_build_derivation()
            .expect("fixed-output authority decodes");
        assert_eq!(request.outputs()[0].hash_algorithm(), algorithm);
        assert_eq!(request.outputs()[0].hash(), hash);
    }
}

#[test]
fn rejects_malformed_paths_unsupported_output_forms_and_build_modes() {
    for wire in [
        request_with_drv_path(b"/tmp/not-a-derivation.drv"),
        request_with_output(b"out", b"not-a-store-path", b"", b""),
        request_with_output(b"out", output_path(), b"sha256", b"00"),
        request_with_output(
            b"out",
            output_path(),
            b"sha256",
            b"gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
        ),
        request_with_output(
            b"out",
            output_path(),
            b"text:sha256",
            b"0000000000000000000000000000000000000000000000000000000000000000",
        ),
        gate_3_request("x86_64-linux", 1),
        gate_3_request("x86_64-linux", 2),
        gate_3_request("x86_64-linux", 3),
    ] {
        let mut reader = reader(&wire);
        assert_eq!(
            reader.read_operation().unwrap(),
            WorkerOperation::BuildDerivation
        );
        assert!(reader.complete_build_derivation().is_err());
    }
}

#[test]
fn rejects_oversized_strings_truncation_and_nonzero_padding() {
    let oversized = "x".repeat(65 * 1024);
    let mut oversized_wire = request_prefix(drv_path());
    write_worker_integer(&mut oversized_wire, 0);
    write_worker_integer(&mut oversized_wire, 0);
    write_worker_byte_string(&mut oversized_wire, b"x86_64-linux");
    write_worker_byte_string(&mut oversized_wire, b"/bin/sh");
    write_worker_integer(&mut oversized_wire, 1);
    write_worker_byte_string(&mut oversized_wire, oversized.as_bytes());
    write_worker_integer(&mut oversized_wire, 0);
    write_worker_integer(&mut oversized_wire, 0);
    assert_invalid(oversized_wire);

    let mut truncated = gate_3_request("x86_64-linux", 0);
    truncated.pop();
    assert_invalid(truncated);

    let mut padding = request_prefix(drv_path());
    write_worker_integer(&mut padding, 0);
    write_worker_integer(&mut padding, 0);
    write_worker_integer(&mut padding, 1);
    padding.push(b'x');
    padding.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1]);
    assert_invalid(padding);
}

#[derive(Clone, Copy, Debug)]
enum CountField {
    Outputs,
    InputSources,
    Arguments,
    Environment,
}

fn oversized_count_request(field: CountField) -> Vec<u8> {
    let mut wire = request_prefix(drv_path());
    if matches!(field, CountField::Outputs) {
        write_worker_integer(&mut wire, u64::MAX);
        return wire;
    }
    write_worker_integer(&mut wire, 0);
    if matches!(field, CountField::InputSources) {
        write_worker_integer(&mut wire, u64::MAX);
        return wire;
    }
    write_worker_integer(&mut wire, 0);
    write_worker_byte_string(&mut wire, b"x86_64-linux");
    write_worker_byte_string(&mut wire, b"/bin/sh");
    if matches!(field, CountField::Arguments) {
        write_worker_integer(&mut wire, u64::MAX);
        return wire;
    }
    write_worker_integer(&mut wire, 0);
    write_worker_integer(&mut wire, u64::MAX);
    wire
}

fn request_with_duplicate_output() -> Vec<u8> {
    let mut wire = request_prefix(drv_path());
    write_worker_integer(&mut wire, 2);
    append_output(&mut wire, b"out", output_path(), b"", b"");
    append_output(&mut wire, b"out", output_path(), b"", b"");
    append_tail(&mut wire, &[], "x86_64-linux", &[], &[], 0);
    wire
}

fn request_with_duplicate_input() -> Vec<u8> {
    let input = b"/nix/store/00000000000000000000000000000000-input";
    let mut wire = request_prefix(drv_path());
    write_worker_integer(&mut wire, 0);
    append_tail(
        &mut wire,
        &[input.as_slice(), input.as_slice()],
        "x86_64-linux",
        &[],
        &[],
        0,
    );
    wire
}

fn request_with_duplicate_environment_key() -> Vec<u8> {
    let mut wire = request_prefix(drv_path());
    write_worker_integer(&mut wire, 0);
    append_tail(
        &mut wire,
        &[],
        "x86_64-linux",
        &[],
        &[(b"name", b"one"), (b"name", b"two")],
        0,
    );
    wire
}

fn request_with_drv_path(path: &[u8]) -> Vec<u8> {
    let mut wire = request_prefix(path);
    write_worker_integer(&mut wire, 0);
    append_tail(&mut wire, &[], "x86_64-linux", &[], &[], 0);
    wire
}

fn request_with_output(name: &[u8], path: &[u8], algorithm: &[u8], hash: &[u8]) -> Vec<u8> {
    let mut wire = request_prefix(drv_path());
    write_worker_integer(&mut wire, 1);
    append_output(&mut wire, name, path, algorithm, hash);
    append_tail(&mut wire, &[], "x86_64-linux", &[], &[], 0);
    wire
}

fn gate_3_request(system: &str, mode: u64) -> Vec<u8> {
    let mut wire = request_prefix(drv_path());
    write_worker_integer(&mut wire, 1);
    append_output(&mut wire, b"out", output_path(), b"", b"");
    append_tail(
        &mut wire,
        &[],
        system,
        &[b"-c", b"printf telchar-remote-build > $out"],
        &[
            (b"builder", b"/bin/sh"),
            (b"name", b"telchar-gate-3-contract"),
            (b"out", output_path()),
            (b"system", system.as_bytes()),
        ],
        mode,
    );
    wire
}

fn request_prefix(path: &[u8]) -> Vec<u8> {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 36);
    write_worker_byte_string(&mut wire, path);
    wire
}

fn append_output(wire: &mut Vec<u8>, name: &[u8], path: &[u8], algorithm: &[u8], hash: &[u8]) {
    write_worker_byte_string(wire, name);
    write_worker_byte_string(wire, path);
    write_worker_byte_string(wire, algorithm);
    write_worker_byte_string(wire, hash);
}

fn append_tail(
    wire: &mut Vec<u8>,
    inputs: &[&[u8]],
    system: &str,
    arguments: &[&[u8]],
    environment: &[(&[u8], &[u8])],
    mode: u64,
) {
    write_worker_integer(wire, inputs.len() as u64);
    for input in inputs {
        write_worker_byte_string(wire, input);
    }
    write_worker_byte_string(wire, system.as_bytes());
    write_worker_byte_string(wire, b"/bin/sh");
    write_worker_integer(wire, arguments.len() as u64);
    for argument in arguments {
        write_worker_byte_string(wire, argument);
    }
    write_worker_integer(wire, environment.len() as u64);
    for (key, value) in environment {
        write_worker_byte_string(wire, key);
        write_worker_byte_string(wire, value);
    }
    write_worker_integer(wire, mode);
}

fn assert_invalid(wire: Vec<u8>) {
    let mut reader = reader(&wire);
    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::BuildDerivation
    );
    assert!(reader.complete_build_derivation().is_err());
}

fn drv_path() -> &'static [u8] {
    b"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"
}

fn output_path() -> &'static [u8] {
    b"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"
}

fn reader(wire: &[u8]) -> WorkerReader<&[u8]> {
    WorkerReader::new(
        wire,
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    )
}
