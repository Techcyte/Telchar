use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::thread;

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, SERVER_WORKER_MAGIC, STDERR_LAST, WorkerOperation,
    write_worker_byte_string, write_worker_integer,
};
use telchar::worker_trace::TraceCapture;

#[test]
fn captures_real_nix_worker_handshake_and_operation_without_payloads() {
    let capture = TraceCapture::start("/nix/var/nix/daemon-socket/socket").expect("capture starts");

    let output = std::process::Command::new("nix")
        .args(["--store", &capture.store_url(), "store", "info", "--json"])
        .output()
        .expect("real Nix client runs");
    assert!(output.status.success(), "Nix client failed: {output:?}");

    let trace = capture.finish().expect("capture finishes");
    assert_eq!(trace.client_protocol_version(), (1, 38));
    assert_eq!(trace.peer_protocol_version(), (1, 38));
    assert_eq!(trace.operations(), &[WorkerOperation::SetOptions]);
    assert!(!trace.contains_payloads());
    assert_eq!(
        trace.sanitized_json(),
        "{\"client_protocol\":\"1.38\",\"operations\":[SetOptions],\"peer_protocol\":\"1.38\"}"
    );
}

#[test]
fn rejects_unknown_operation_without_relaying_its_body() {
    let peer_path = std::env::temp_dir().join(format!(
        "telchar-worker-trace-peer-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let listener = UnixListener::bind(&peer_path).expect("peer listener binds");
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("trace peer connects");
        assert_eq!(read_word(&mut stream), CLIENT_WORKER_MAGIC);
        write_word(&mut stream, SERVER_WORKER_MAGIC);
        write_word(&mut stream, 0x126);
        assert_eq!(read_word(&mut stream), 0x126);
        assert_eq!(read_word(&mut stream), 0);
        write_word(&mut stream, 0);
        assert_eq!(read_word(&mut stream), 0);
        assert_eq!(read_word(&mut stream), 0);
        let mut daemon_version = Vec::new();
        write_worker_byte_string(&mut daemon_version, b"daemon-secret");
        stream
            .write_all(&daemon_version)
            .expect("daemon version writes");
        write_word(&mut stream, 0);
        write_word(&mut stream, STDERR_LAST);
        assert_eq!(read_word(&mut stream), 999);
        let mut body = [0; 1];
        assert_eq!(stream.read(&mut body).expect("peer observes close"), 0);
    });

    let capture =
        TraceCapture::start(peer_path.to_str().expect("UTF-8 path")).expect("capture starts");
    let socket_path = capture
        .store_url()
        .strip_prefix("unix://")
        .unwrap()
        .to_owned();
    let mut client = std::os::unix::net::UnixStream::connect(socket_path).expect("client connects");
    write_word(&mut client, CLIENT_WORKER_MAGIC);
    assert_eq!(read_word(&mut client), SERVER_WORKER_MAGIC);
    assert_eq!(read_word(&mut client), 0x126);
    write_word(&mut client, 0x126);
    write_word(&mut client, 0);
    assert_eq!(read_word(&mut client), 0);
    write_word(&mut client, 0);
    write_word(&mut client, 0);
    discard_string(&mut client);
    assert_eq!(read_word(&mut client), 0);
    assert_eq!(read_word(&mut client), STDERR_LAST);
    write_word(&mut client, 999);
    client.write_all(b"must-not-relay").expect("body writes");
    drop(client);

    let error = capture.finish().expect_err("unknown operation is rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "untyped worker operation");
    peer.join().expect("peer completes");
    let _ = std::fs::remove_file(peer_path);
}

fn write_word(stream: &mut impl Write, value: u64) {
    let mut bytes = Vec::new();
    write_worker_integer(&mut bytes, value);
    stream.write_all(&bytes).expect("word writes");
}

fn read_word(stream: &mut impl Read) -> u64 {
    let mut bytes = [0; 8];
    stream.read_exact(&mut bytes).expect("word reads");
    u64::from_le_bytes(bytes)
}

fn discard_string(stream: &mut impl Read) {
    let length = read_word(stream) as usize;
    let mut bytes = vec![0; length + (8 - length % 8) % 8];
    stream.read_exact(&mut bytes).expect("string reads");
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_nanos()
}
