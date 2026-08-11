use std::io;
use std::time::Duration;

use nix_worker_protocol::{
    write_worker_byte_string, write_worker_integer, ProtocolSessionLimits, WorkerReader,
};
use telchar::build_request::BuildRequest;
use telchar::deployment::DeploymentConfig;

#[test]
fn normalizes_gate_3_request_without_backend_objects() {
    let worker = decode_gate_3_request("x86_64-linux", 0);
    let deployment = DeploymentConfig::parse("x86_64-linux", "").expect("deployment config parses");

    let request = BuildRequest::from_worker_request(&worker, &deployment)
        .expect("Gate 3 request is admitted");

    assert!(request
        .derivation_path()
        .ends_with(b"-telchar-gate-3-contract.drv"));
    assert_eq!(request.expected_outputs().len(), 1);
    assert_eq!(request.expected_outputs()[0].0, b"out");
    assert_eq!(request.system(), "x86_64-linux");
    assert_eq!(request.builder(), b"/bin/sh");
    assert_eq!(request.arguments().len(), 2);
    assert_eq!(request.environment().len(), 4);
    assert!(request.required_system_features().is_empty());
    assert!(request.input_sources().is_empty());
}

#[test]
fn preserves_bounded_required_system_features() {
    let worker = decode_request_with_features("kvm big-parallel");
    let deployment = DeploymentConfig::parse("x86_64-linux", "kvm,big-parallel")
        .expect("deployment config parses");

    let request = BuildRequest::from_worker_request(&worker, &deployment)
        .expect("required features are admitted");

    assert_eq!(request.required_system_features(), ["big-parallel", "kvm"]);
}

#[test]
fn rejects_unsupported_or_malformed_required_system_features() {
    let deployment =
        DeploymentConfig::parse("x86_64-linux", "kvm").expect("deployment config parses");

    for features in ["benchmark", "kvm kvm", "kvm feature/unsafe"] {
        let worker = decode_request_with_features(features);
        assert_eq!(
            BuildRequest::from_worker_request(&worker, &deployment)
                .expect_err("invalid required features must fail")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn equivalent_requests_have_the_same_shared_build_key() {
    let deployment = DeploymentConfig::parse("x86_64-linux", "").expect("deployment parses");
    let first =
        BuildRequest::from_worker_request(&decode_gate_3_request("x86_64-linux", 0), &deployment)
            .expect("first request admits");
    let second =
        BuildRequest::from_worker_request(&decode_gate_3_request("x86_64-linux", 0), &deployment)
            .expect("second request admits");

    assert_eq!(first.shared_build_key(), second.shared_build_key());
    assert_eq!(first.shared_build_key().len(), drv_path().len() + 1 + 64);
}

#[test]
fn admitted_semantic_difference_changes_shared_build_key() {
    let deployment = DeploymentConfig::parse("x86_64-linux", "").expect("deployment parses");
    let first =
        BuildRequest::from_worker_request(&decode_gate_3_request("x86_64-linux", 0), &deployment)
            .expect("first request admits");
    let second = BuildRequest::from_worker_request(
        &decode_request_with_command("printf different > $out"),
        &deployment,
    )
    .expect("second request admits");

    assert_ne!(first.shared_build_key(), second.shared_build_key());
}

#[test]
fn rejects_system_mismatch_before_execution() {
    let worker = decode_gate_3_request("aarch64-linux", 0);
    let deployment = DeploymentConfig::parse("x86_64-linux", "").expect("deployment config parses");

    let error = BuildRequest::from_worker_request(&worker, &deployment)
        .expect_err("mismatched system must fail admission");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn rejects_output_environment_mismatch() {
    let worker = decode_request(
        "x86_64-linux",
        b"/nix/store/22222222222222222222222222222222-different-output",
        0,
    );
    let deployment = DeploymentConfig::parse("x86_64-linux", "").expect("deployment config parses");

    assert_eq!(
        BuildRequest::from_worker_request(&worker, &deployment)
            .expect_err("output environment mismatch must fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn rejects_non_normal_build_modes() {
    for mode in [1, 2] {
        let wire = build_request_wire("x86_64-linux", output_path(), mode);
        let mut reader = WorkerReader::new(
            wire.as_slice(),
            ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
        );
        assert_eq!(
            reader
                .read_operation()
                .expect("BuildDerivation operation reads"),
            nix_worker_protocol::WorkerOperation::BuildDerivation
        );
        let error = reader
            .complete_build_derivation()
            .expect_err("non-normal build mode must fail before admission");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}

fn decode_gate_3_request(system: &str, mode: u64) -> nix_worker_protocol::BuildDerivationRequest {
    decode_request(system, output_path(), mode)
}

fn decode_request(
    system: &str,
    environment_output: &[u8],
    mode: u64,
) -> nix_worker_protocol::BuildDerivationRequest {
    let wire = build_request_wire(system, environment_output, mode);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );
    let operation = reader.read_operation().expect("operation reads");
    assert_eq!(
        operation,
        nix_worker_protocol::WorkerOperation::BuildDerivation
    );
    reader
        .complete_build_derivation()
        .expect("worker request decodes for admission test")
}

fn decode_request_with_features(features: &str) -> nix_worker_protocol::BuildDerivationRequest {
    let wire = build_request_wire_with_environment(
        "x86_64-linux",
        output_path(),
        0,
        "printf telchar-remote-build > $out",
        Some(features),
    );
    decode_wire(wire)
}

fn decode_request_with_command(command: &str) -> nix_worker_protocol::BuildDerivationRequest {
    let wire = build_request_wire_with_command("x86_64-linux", output_path(), 0, command);
    decode_wire(wire)
}

fn decode_wire(wire: Vec<u8>) -> nix_worker_protocol::BuildDerivationRequest {
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );
    assert_eq!(
        reader.read_operation().expect("operation reads"),
        nix_worker_protocol::WorkerOperation::BuildDerivation
    );
    reader
        .complete_build_derivation()
        .expect("worker request decodes")
}

fn build_request_wire(system: &str, environment_output: &[u8], mode: u64) -> Vec<u8> {
    build_request_wire_with_command(
        system,
        environment_output,
        mode,
        "printf telchar-remote-build > $out",
    )
}

fn build_request_wire_with_command(
    system: &str,
    environment_output: &[u8],
    mode: u64,
    command: &str,
) -> Vec<u8> {
    build_request_wire_with_environment(system, environment_output, mode, command, None)
}

fn build_request_wire_with_environment(
    system: &str,
    environment_output: &[u8],
    mode: u64,
    command: &str,
    required_system_features: Option<&str>,
) -> Vec<u8> {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 36);
    write_worker_byte_string(&mut wire, drv_path());
    write_worker_integer(&mut wire, 1);
    write_worker_byte_string(&mut wire, b"out");
    write_worker_byte_string(&mut wire, output_path());
    write_worker_byte_string(&mut wire, b"");
    write_worker_byte_string(&mut wire, b"");
    write_worker_integer(&mut wire, 0);
    write_worker_byte_string(&mut wire, system.as_bytes());
    write_worker_byte_string(&mut wire, b"/bin/sh");
    write_worker_integer(&mut wire, 2);
    write_worker_byte_string(&mut wire, b"-c");
    write_worker_byte_string(&mut wire, command.as_bytes());
    write_worker_integer(&mut wire, 4 + u64::from(required_system_features.is_some()));
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"telchar-gate-3-contract".as_slice()),
        (b"out".as_slice(), environment_output),
        (b"system".as_slice(), system.as_bytes()),
    ] {
        write_worker_byte_string(&mut wire, key);
        write_worker_byte_string(&mut wire, value);
    }
    if let Some(features) = required_system_features {
        write_worker_byte_string(&mut wire, b"requiredSystemFeatures");
        write_worker_byte_string(&mut wire, features.as_bytes());
    }
    write_worker_integer(&mut wire, mode);
    wire
}

fn drv_path() -> &'static [u8] {
    b"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"
}

fn output_path() -> &'static [u8] {
    b"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"
}
