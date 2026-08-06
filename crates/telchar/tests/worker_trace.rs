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
fn relays_a_fixture_sized_add_to_store_upload_byte_for_byte_with_a_bounded_buffer() {
    let peer_path = temporary_socket_path("upload");
    let listener = UnixListener::bind(&peer_path).expect("peer listener binds");
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("trace peer connects");
        complete_fixture_handshake(&mut stream);
        receive_set_options(&mut stream);
        send_words(&mut stream, &[STDERR_LAST]);
        receive_store_path_operation(&mut stream, 11);
        send_words(&mut stream, &[STDERR_LAST, 1]);
        receive_store_path_operation(&mut stream, 1);
        send_words(&mut stream, &[STDERR_LAST, 0]);
        let mut request = Vec::new();
        receive_word(&mut stream, &mut request);
        receive_string(&mut stream, &mut request);
        receive_string(&mut stream, &mut request);
        receive_word(&mut stream, &mut request);
        receive_word(&mut stream, &mut request);
        receive_word(&mut stream, &mut request);
        let mut upload = [0; 502];
        stream.read_exact(&mut upload).expect("upload body reads");
        request.extend(upload);
        receive_word(&mut stream, &mut request);
        send_words(&mut stream, &[STDERR_LAST]);
        send_valid_path_info(&mut stream);
        receive_derived_paths_operation(&mut stream, 40, false);
        send_words(&mut stream, &[STDERR_LAST, 1]);
        stream
            .write_all(&worker_string(b"/fixture/store-path"))
            .expect("will-build path writes");
        send_words(&mut stream, &[0, 0, 0, 0]);
        receive_store_path_operation(&mut stream, 26);
        send_words(&mut stream, &[STDERR_LAST, 0]);
        receive_derived_paths_operation(&mut stream, 46, true);
        send_words(&mut stream, &[STDERR_LAST]);
        send_build_result(&mut stream);
        request
    });

    let capture =
        TraceCapture::start(peer_path.to_str().expect("UTF-8 path")).expect("capture starts");
    let socket_path = capture
        .store_url()
        .strip_prefix("unix://")
        .unwrap()
        .to_owned();
    let mut client = std::os::unix::net::UnixStream::connect(socket_path).expect("client connects");
    complete_fixture_client_handshake(&mut client);
    send_set_options(&mut client);
    assert_eq!(read_word(&mut client), STDERR_LAST);
    send_store_path_operation(&mut client, 11);
    assert_eq!(read_word(&mut client), STDERR_LAST);
    assert_eq!(read_word(&mut client), 1);
    send_store_path_operation(&mut client, 1);
    assert_eq!(read_word(&mut client), STDERR_LAST);
    assert_eq!(read_word(&mut client), 0);

    let mut expected = worker_words(&[7]);
    expected.extend(worker_string(b"fixture-upload"));
    expected.extend(worker_string(b"fixed:sha25"));
    append_words(&mut expected, &[0, 0, 502]);
    expected.extend([0x5a; 502]);
    append_words(&mut expected, &[0]);
    client.write_all(&expected).expect("upload request writes");
    assert_eq!(read_word(&mut client), STDERR_LAST);
    discard_valid_path_info(&mut client);
    send_derived_paths_operation(&mut client, 40, false);
    assert_eq!(read_word(&mut client), STDERR_LAST);
    assert_eq!(read_word(&mut client), 1);
    discard_string(&mut client);
    for _ in 0..4 {
        read_word(&mut client);
    }
    send_store_path_operation(&mut client, 26);
    assert_eq!(read_word(&mut client), STDERR_LAST);
    assert_eq!(read_word(&mut client), 0);
    send_derived_paths_operation(&mut client, 46, true);
    assert_eq!(read_word(&mut client), STDERR_LAST);
    discard_build_result(&mut client);
    drop(client);

    assert_eq!(peer.join().expect("peer completes"), expected);
    let trace = capture.finish().expect("capture finishes");
    assert_eq!(
        trace.operations(),
        &[
            WorkerOperation::SetOptions,
            WorkerOperation::AddTempRoot,
            WorkerOperation::IsValidPath,
            WorkerOperation::AddToStore,
            WorkerOperation::QueryMissing,
            WorkerOperation::QueryPathInfo,
            WorkerOperation::BuildPathsWithResults,
        ]
    );
    assert!(!trace.contains_payloads());
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

fn complete_fixture_handshake(stream: &mut (impl Read + Write)) {
    assert_eq!(read_word(stream), CLIENT_WORKER_MAGIC);
    send_words(stream, &[SERVER_WORKER_MAGIC, 0x126]);
    assert_eq!(read_word(stream), 0x126);
    assert_eq!(read_word(stream), 0);
    send_words(stream, &[0]);
    assert_eq!(read_word(stream), 0);
    assert_eq!(read_word(stream), 0);
    stream
        .write_all(&worker_string(b"daemon"))
        .expect("daemon version writes");
    send_words(stream, &[0, STDERR_LAST]);
}

fn complete_fixture_client_handshake(stream: &mut (impl Read + Write)) {
    write_word(stream, CLIENT_WORKER_MAGIC);
    assert_eq!(read_word(stream), SERVER_WORKER_MAGIC);
    assert_eq!(read_word(stream), 0x126);
    send_words(stream, &[0x126, 0]);
    assert_eq!(read_word(stream), 0);
    send_words(stream, &[0, 0]);
    discard_string(stream);
    assert_eq!(read_word(stream), 0);
    assert_eq!(read_word(stream), STDERR_LAST);
}

fn receive_set_options(stream: &mut impl Read) {
    assert_eq!(read_word(stream), 19);
    for _ in 0..12 {
        read_word(stream);
    }
    assert_eq!(read_word(stream), 0);
}

fn send_set_options(stream: &mut impl Write) {
    send_words(stream, &[19]);
    send_words(stream, &[0; 12]);
    send_words(stream, &[0]);
}

fn receive_store_path_operation(stream: &mut impl Read, operation: u64) {
    assert_eq!(read_word(stream), operation);
    discard_string(stream);
}

fn send_store_path_operation(stream: &mut impl Write, operation: u64) {
    send_words(stream, &[operation]);
    stream
        .write_all(&worker_string(b"/fixture/store-path"))
        .expect("store path writes");
}

fn send_valid_path_info(stream: &mut impl Write) {
    stream
        .write_all(&worker_string(b"/fixture/store-path"))
        .expect("path writes");
    stream
        .write_all(&worker_string(b""))
        .expect("deriver writes");
    stream
        .write_all(&worker_string(
            b"sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ))
        .expect("hash writes");
    send_words(stream, &[0, 0, 0, 0, 0]);
    stream
        .write_all(&worker_string(b"fixed:sha256"))
        .expect("content address writes");
}

fn discard_valid_path_info(stream: &mut impl Read) {
    discard_string(stream);
    discard_string(stream);
    discard_string(stream);
    read_word(stream);
    read_word(stream);
    read_word(stream);
    read_word(stream);
    read_word(stream);
    discard_string(stream);
}

fn receive_derived_paths_operation(stream: &mut impl Read, operation: u64, mode: bool) {
    assert_eq!(read_word(stream), operation);
    assert_eq!(read_word(stream), 1);
    discard_string(stream);
    if mode {
        assert!(read_word(stream) <= 2);
    }
}

fn send_derived_paths_operation(stream: &mut impl Write, operation: u64, mode: bool) {
    send_words(stream, &[operation, 1]);
    stream
        .write_all(&worker_string(b"/fixture/derived-path"))
        .expect("derived path writes");
    if mode {
        send_words(stream, &[0]);
    }
}

fn send_build_result(stream: &mut impl Write) {
    send_words(stream, &[1]);
    stream
        .write_all(&worker_string(b"/fixture/derived-path"))
        .expect("result path writes");
    send_words(stream, &[0]);
    stream.write_all(&worker_string(b"")).expect("error writes");
    send_words(stream, &[0, 0, 0, 0, 0, 0, 1]);
    stream
        .write_all(&worker_string(b"out"))
        .expect("output id writes");
    stream
        .write_all(&worker_string(b"/fixture/output-realisation"))
        .expect("realisation writes");
}

fn discard_build_result(stream: &mut impl Read) {
    assert_eq!(read_word(stream), 1);
    discard_string(stream);
    read_word(stream);
    discard_string(stream);
    for _ in 0..6 {
        read_word(stream);
    }
    assert_eq!(read_word(stream), 1);
    discard_string(stream);
    discard_string(stream);
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
