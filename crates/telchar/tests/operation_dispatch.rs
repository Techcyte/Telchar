use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_ERROR, STDERR_LAST,
};

#[test]
fn live_set_options_request_returns_terminal_frame() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .arg("serve-stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Telchar starts");
    let mut input = child.stdin.take().expect("server input");

    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 19);
    for _ in 0..12 {
        write_integer(&mut input, 0);
    }
    write_integer(&mut input, 0);
    drop(input);

    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .expect("server stdout")
        .read_to_end(&mut stdout)
        .expect("server stdout reads");
    let mut expected_stdout = Vec::new();
    write_integer(&mut expected_stdout, SERVER_WORKER_MAGIC);
    write_integer(&mut expected_stdout, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut expected_stdout, 0);
    write_string(&mut expected_stdout, b"telchar");
    write_integer(&mut expected_stdout, 0);
    write_integer(&mut expected_stdout, STDERR_LAST);
    write_integer(&mut expected_stdout, STDERR_LAST);
    assert_eq!(
        stdout, expected_stdout,
        "worker stdout has no text contamination"
    );

    assert!(child.wait().expect("Telchar exits").success());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("server stderr")
        .read_to_string(&mut stderr)
        .expect("server stderr reads");
    assert!(
        stderr.contains("worker.set_options.completed"),
        "missing local SetOptions telemetry: {stderr}"
    );
}

#[test]
fn partial_set_options_times_out_after_operation_and_cleans_up() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .arg("serve-stdio")
        .env("TELCHAR_WORKER_IDLE_TIMEOUT_MS", "40")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Telchar starts");
    let mut input = child.stdin.take().expect("server input");
    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    input.flush().expect("handshake flushes");
    let mut output = child.stdout.take().expect("server output");
    assert_eq!(read_integer(&mut output), SERVER_WORKER_MAGIC);
    assert_eq!(read_integer(&mut output), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_integer(&mut output), 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().expect("post-handshake flushes");
    assert_eq!(read_string(&mut output), "telchar");
    assert_eq!(read_integer(&mut output), 0);
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    write_integer(&mut input, 19);
    input.flush().expect("operation flushes");
    let started = std::time::Instant::now();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("server stderr")
        .read_to_string(&mut stderr)
        .expect("server stderr reads");
    let elapsed = started.elapsed();
    let status = child.wait().expect("Telchar exits");
    assert!(elapsed < Duration::from_secs(1));
    assert!(status.success());
    assert!(stderr.contains("worker.session.timed_out"), "{stderr}");
}

#[test]
fn partial_set_options_progress_resets_deadline() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .arg("serve-stdio")
        .env("TELCHAR_WORKER_IDLE_TIMEOUT_MS", "40")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Telchar starts");
    let mut input = child.stdin.take().expect("server input");
    write_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut input, 0);
    input.flush().expect("handshake flushes");
    let mut output = child.stdout.take().expect("server output");
    assert_eq!(read_integer(&mut output), SERVER_WORKER_MAGIC);
    assert_eq!(read_integer(&mut output), LATEST_WORKER_VERSION.to_wire());
    assert_eq!(read_integer(&mut output), 0);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().expect("post-handshake flushes");
    assert_eq!(read_string(&mut output), "telchar");
    assert_eq!(read_integer(&mut output), 0);
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    write_integer(&mut input, 19);
    write_integer(&mut input, 0);
    input.flush().expect("partial request progresses");
    std::thread::sleep(Duration::from_millis(25));
    for _ in 0..11 {
        write_integer(&mut input, 0);
    }
    write_integer(&mut input, 0);
    input.flush().expect("request completes");
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
}

#[test]
fn recognized_unsupported_operation_returns_a_distinct_framed_error() {
    let response = send_operation(39);
    assert_eq!(response.message, "unsupported worker operation");
    assert_eq!(response.rejection, "recognized-unsupported");
}

#[test]
fn unknown_operation_returns_a_framed_error() {
    let response = send_operation(0xffff);
    assert_eq!(response.message, "unknown worker operation");
    assert_eq!(response.rejection, "unknown-operation");
}

struct OperationResponse {
    message: String,
    rejection: &'static str,
}

fn send_operation(operation: u64) -> OperationResponse {
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

    write_integer(&mut input, operation);
    input.flush().expect("operation flushes");
    drop(input);

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    let message = read_string(&mut output);
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
    let rejection = if stderr.contains("recognized-unsupported") {
        "recognized-unsupported"
    } else {
        "unknown-operation"
    };
    OperationResponse { message, rejection }
}

fn write_integer(output: &mut impl Write, value: u64) {
    output
        .write_all(&value.to_le_bytes())
        .expect("worker integer writes");
}

fn write_string(output: &mut impl Write, value: &[u8]) {
    write_integer(output, value.len() as u64);
    output.write_all(value).expect("worker string writes");
    output
        .write_all(&[0; 7][..(8 - value.len() % 8) % 8])
        .expect("worker string padding writes");
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
