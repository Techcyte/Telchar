use super::*;
use std::io::Write;

#[test]
fn catalog_transport_rejects_errors_redirects_and_invalid_bodies() {
    for (status, body) in [
        ("503 Service Unavailable", "{}"),
        ("302 Found", "{}"),
        ("200 OK", "not-json"),
        ("200 OK", "{}"),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "catalog client did not connect before fixture deadline"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("catalog fixture accept failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 16384);
            }
            write!(stream, "HTTP/1.1 {status}\r\nLocation: http://127.0.0.1:1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        assert!(fetch_catalog(&endpoint, "us-west-2", 2048, Duration::from_secs(2)).is_err());
        server.join().unwrap();
    }
}
