//! Provides shared integration-test helpers for build request scenarios.

use nix_worker_protocol::{ProtocolSessionLimits, WorkerReader};
use telchar::backend::{BackendKind, BackendTarget};
use telchar::build::BuildRequest;

pub fn admitted_request() -> BuildRequest {
    let output = b"/nix/store/11111111111111111111111111111111-static-ssh-output";
    let mut wire = Vec::new();
    write_string(
        &mut wire,
        b"/nix/store/00000000000000000000000000000000-static-ssh.drv",
    );
    write_integer(&mut wire, 1);
    write_string(&mut wire, b"out");
    write_string(&mut wire, output);
    write_string(&mut wire, b"");
    write_string(&mut wire, b"");
    write_integer(&mut wire, 0);
    write_string(&mut wire, b"x86_64-linux");
    write_string(&mut wire, b"/bin/sh");
    write_integer(&mut wire, 0);
    write_integer(&mut wire, 4);
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"static-ssh".as_slice()),
        (b"out".as_slice(), output.as_slice()),
        (b"system".as_slice(), b"x86_64-linux".as_slice()),
    ] {
        write_string(&mut wire, key);
        write_string(&mut wire, value);
    }
    write_integer(&mut wire, 0);
    let mut reader = WorkerReader::new(wire.as_slice(), ProtocolSessionLimits::DEFAULT);
    let worker = reader
        .complete_build_derivation()
        .expect("worker request parses");
    let backends = [BackendTarget::new(
        "fixture",
        BackendKind::Local,
        "x86_64-linux",
        [] as [&str; 0],
    )
    .expect("backend parses")];
    BuildRequest::from_worker_request(&worker, &backends).expect("request admits")
}

fn write_integer(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_string(output: &mut Vec<u8>, value: &[u8]) {
    write_integer(output, value.len() as u64);
    output.extend_from_slice(value);
    output.resize(output.len() + (8 - value.len() % 8) % 8, 0);
}
