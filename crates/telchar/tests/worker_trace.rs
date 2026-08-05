use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::thread;

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, SERVER_WORKER_MAGIC, STDERR_LAST, WorkerOperation,
    write_worker_byte_string, write_worker_integer,
};
use telchar::worker_trace::{TRACE_RELAY_BUFFER_BYTES, TraceCapture};

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
fn relays_typed_fixture_messages_byte_for_byte_without_retaining_bodies() {
    let peer_path = temporary_socket_path("peer");
    let listener = UnixListener::bind(&peer_path).expect("peer listener binds");
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("trace peer connects");
        let mut received = Vec::new();

        receive_word(&mut stream, &mut received);
        send_words(&mut stream, &[SERVER_WORKER_MAGIC, 0x126]);
        receive_words(&mut stream, &mut received, 2);
        send_words(&mut stream, &[0]);
        receive_words(&mut stream, &mut received, 2);
        let mut handshake_info = worker_string(b"daemon-secret");
        append_words(&mut handshake_info, &[0, STDERR_LAST]);
        stream
            .write_all(&handshake_info)
            .expect("handshake info writes");
        receive_words(&mut stream, &mut received, 14);
        receive_string(&mut stream, &mut received);
        receive_string(&mut stream, &mut received);
        send_words(&mut stream, &[STDERR_LAST]);

        received
    });

    let capture =
        TraceCapture::start(peer_path.to_str().expect("UTF-8 path")).expect("capture starts");
    let socket_path = capture
        .store_url()
        .strip_prefix("unix://")
        .unwrap()
        .to_owned();
    let mut client = std::os::unix::net::UnixStream::connect(socket_path).expect("client connects");
    let mut expected_client = Vec::new();

    send_word_and_record(&mut client, CLIENT_WORKER_MAGIC, &mut expected_client);
    assert_eq!(read_word(&mut client), SERVER_WORKER_MAGIC);
    assert_eq!(read_word(&mut client), 0x126);
    send_words_and_record(&mut client, &[0x126, 0], &mut expected_client);
    assert_eq!(read_word(&mut client), 0);
    send_words_and_record(&mut client, &[0, 0], &mut expected_client);

    let mut expected_server = worker_string(b"daemon-secret");
    append_words(&mut expected_server, &[0, STDERR_LAST]);
    let mut received_server = vec![0; expected_server.len()];
    client
        .read_exact(&mut received_server)
        .expect("handshake info reads");
    assert_eq!(received_server, expected_server);

    let mut set_options = worker_words(&[19]);
    append_words(&mut set_options, &[0; 12]);
    append_words(&mut set_options, &[1]);
    set_options.extend(worker_string(b"secret-name"));
    set_options.extend(worker_string(b"secret-value"));
    expected_client.extend(&set_options);
    client.write_all(&set_options).expect("set options writes");
    assert_eq!(read_word(&mut client), STDERR_LAST);
    drop(client);

    let received_client = peer.join().expect("peer completes");
    let trace = capture.finish().expect("capture finishes");
    assert_eq!(received_client, expected_client);
    assert_eq!(hash(&received_client), hash(&expected_client));
    assert_eq!(trace.operations(), &[WorkerOperation::SetOptions]);
    assert!(!trace.sanitized_json().contains("secret"));
    assert!(std::hint::black_box(TRACE_RELAY_BUFFER_BYTES) <= 4096);
    let _ = std::fs::remove_file(peer_path);
}

#[test]
fn rejects_unknown_operation_without_relaying_its_body() {
    let peer_path = temporary_socket_path("unknown");
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
        stream
            .write_all(&worker_string(b"daemon-secret"))
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

fn send_word_and_record(stream: &mut impl Write, value: u64, record: &mut Vec<u8>) {
    let encoded = worker_words(&[value]);
    stream.write_all(&encoded).expect("word writes");
    record.extend(encoded);
}

fn send_words_and_record(stream: &mut impl Write, values: &[u64], record: &mut Vec<u8>) {
    let encoded = worker_words(values);
    stream.write_all(&encoded).expect("words write");
    record.extend(encoded);
}

fn send_words(stream: &mut impl Write, values: &[u64]) {
    stream
        .write_all(&worker_words(values))
        .expect("words write");
}

fn receive_words(stream: &mut impl Read, record: &mut Vec<u8>, count: usize) {
    for _ in 0..count {
        receive_word(stream, record);
    }
}

fn receive_word(stream: &mut impl Read, record: &mut Vec<u8>) {
    let mut encoded = [0; 8];
    stream.read_exact(&mut encoded).expect("word reads");
    record.extend(encoded);
}

fn receive_string(stream: &mut impl Read, record: &mut Vec<u8>) {
    receive_word(stream, record);
    let length = u64::from_le_bytes(record[record.len() - 8..].try_into().expect("word")) as usize;
    let mut body = vec![0; length + (8 - length % 8) % 8];
    stream.read_exact(&mut body).expect("string reads");
    record.extend(body);
}

fn write_word(stream: &mut impl Write, value: u64) {
    stream
        .write_all(&worker_words(&[value]))
        .expect("word writes");
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

fn worker_words(values: &[u64]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in values {
        write_worker_integer(&mut bytes, *value);
    }
    bytes
}

fn append_words(bytes: &mut Vec<u8>, values: &[u64]) {
    bytes.extend(worker_words(values));
}

fn worker_string(value: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_worker_byte_string(&mut bytes, value);
    bytes
}

fn hash(value: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn temporary_socket_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "telchar-worker-trace-{name}-{}-{}",
        std::process::id(),
        unique_suffix()
    ))
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_nanos()
}
