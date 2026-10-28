use std::net::TcpListener;
use std::time::{Duration, Instant};

#[test]
fn manifest_wait_uses_absolute_session_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let _socket = tungstenite::accept(stream).expect("upgrade");
        std::thread::sleep(Duration::from_secs(1));
    });
    let (socket, _) = tungstenite::connect(format!("ws://{address}")).expect("connect");
    let started = Instant::now();
    let result = telchar_nomad_worker::microvm::receive_session(
        socket,
        8192,
        4096,
        started + Duration::from_millis(150),
    );
    assert!(
        result.is_err(),
        "silent gateway cannot extend manifest wait"
    );
    let elapsed = started.elapsed();
    server.join().expect("server");
    assert!(
        elapsed < Duration::from_millis(700),
        "deadline exceeded: {elapsed:?}"
    );
}
