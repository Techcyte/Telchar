//! Tests ipc buffer contracts and failure boundaries, including slow daemon observes bounded frontend buffer.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

use telchar::service::ipc::{relay_bounded, MAX_FRONTEND_BUFFER_BYTES};

#[test]
fn slow_daemon_observes_bounded_frontend_buffer() {
    let (mut frontend_writer, frontend_reader) = UnixStream::pair().expect("frontend pair");
    let (daemon_reader, daemon_socket) = UnixStream::pair().expect("daemon pair");
    let payload = vec![0x5a; MAX_FRONTEND_BUFFER_BYTES * 32];
    let sender = thread::spawn(move || {
        frontend_writer
            .write_all(&payload)
            .expect("frontend writes payload");
    });
    let relay = thread::spawn(move || relay_bounded(frontend_reader, daemon_reader));

    thread::sleep(Duration::from_millis(20));
    let mut received = Vec::new();
    let mut daemon_socket = daemon_socket;
    daemon_socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("daemon timeout sets");
    daemon_socket
        .read_to_end(&mut received)
        .expect("daemon receives payload");
    sender.join().expect("frontend sender completes");
    let stats = relay
        .join()
        .expect("relay completes")
        .expect("relay succeeds");

    assert_eq!(received.len(), MAX_FRONTEND_BUFFER_BYTES * 32);
    assert_eq!(stats.maximum_buffered_bytes, MAX_FRONTEND_BUFFER_BYTES);
}
