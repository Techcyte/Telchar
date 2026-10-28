use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

pub fn serve_image(listener: &TcpListener, image_arn: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "image inspection not requested");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
        assert!(header.len() < 32768);
    }
    let header = String::from_utf8(header).unwrap().to_ascii_lowercase();
    assert!(
        header.starts_with("get /2025-09-09/microvm-images/"),
        "{header}"
    );
    assert!(header.contains("/versions/1 "), "{header}");
    assert!(header.contains("credential=fixture-key/"));
    let body = serde_json::json!({
        "imageArn": image_arn, "imageVersion": "1", "state": "SUCCESSFUL", "status": "ACTIVE",
        "baseImageArn": "arn:aws:lambda:us-west-2:aws:microvm-image:al2023-1",
        "buildRoleArn": "arn:aws:iam::116392764028:role/fixture", "createdAt": 1,
        "cpuConfigurations": [{"architecture": "ARM_64"}],
        "resources": [{"minimumMemoryInMiB": 2048}]
    })
    .to_string();
    write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
}
