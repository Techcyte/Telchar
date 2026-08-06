use std::io::{Read, Write};
use std::process::{Command, Stdio};

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_ERROR, STDERR_LAST,
};

#[test]
fn unknown_operation_returns_a_framed_error() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .arg("serve-stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Telchar starts");
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");

    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    input.flush().expect("handshake flushes");

    assert_eq!(read_integer(&mut output), SERVER_WORKER_MAGIC);
    assert_eq!(
        read_integer(&mut output),
        LATEST_WORKER_VERSION.to_wire(),
        "server sends its protocol version"
    );
    assert_eq!(read_integer(&mut output), 0, "server has no features");

    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().expect("post-handshake flushes");

    assert_eq!(read_string(&mut output), "telchar");
    assert_eq!(read_integer(&mut output), 0);
    assert_eq!(read_integer(&mut output), STDERR_LAST);

    write_integer(&mut input, 0xffff);
    input.flush().expect("operation flushes");
    drop(input);

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(read_string(&mut output), "unknown worker operation");
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");

    let status = child.wait().expect("Telchar exits");
    assert!(status.success(), "Telchar failed: {status}");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("server stderr")
        .read_to_string(&mut stderr)
        .expect("stderr reads");
    assert!(
        stderr.contains("worker.operation.rejected"),
        "missing structured rejection event: {stderr}"
    );
}

fn write_integer(output: &mut impl Write, value: u64) {
    output
        .write_all(&value.to_le_bytes())
        .expect("worker integer writes");
}

fn read_integer(input: &mut impl Read) -> u64 {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes).expect("worker integer reads");
    u64::from_le_bytes(bytes)
}

fn read_string(input: &mut impl Read) -> String {
    let length = read_integer(input) as usize;
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes).expect("worker string reads");
    let padding = (8 - length % 8) % 8;
    let mut padding_bytes = vec![0; padding];
    input
        .read_exact(&mut padding_bytes)
        .expect("worker padding reads");
    assert!(padding_bytes.iter().all(|byte| *byte == 0));
    String::from_utf8(bytes).expect("worker string UTF-8")
}
