//! Records OTLP HTTP signal endpoints.

#[derive(Clone)]
pub struct HttpCollector {
    endpoint: String,
    pub paths: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl HttpCollector {
    pub fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    pub fn has_all_signals(&self) -> bool {
        let paths = self.paths.lock().expect("HTTP collector paths");
        ["/v1/traces", "/v1/logs", "/v1/metrics"]
            .iter()
            .all(|expected| paths.iter().any(|path| path == expected))
    }
}

pub fn start_http_collector() -> HttpCollector {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("HTTP collector listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking HTTP collector listener");
    let collector = HttpCollector {
        endpoint: format!(
            "http://{}",
            listener.local_addr().expect("HTTP collector address")
        ),
        paths: Default::default(),
    };
    let paths = std::sync::Arc::clone(&collector.paths);
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(_) => return,
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let header_end = loop {
                let Ok(read) = stream.read(&mut buffer) else {
                    break None;
                };
                if read == 0 {
                    break None;
                }
                request.extend_from_slice(&buffer[..read]);
                if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    break Some(position + 4);
                }
                if request.len() > 64 * 1024 {
                    break None;
                }
            };
            let Some(header_end) = header_end else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let path = headers
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("")
                .to_owned();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while request.len().saturating_sub(header_end) < content_length {
                let Ok(read) = stream.read(&mut buffer) else {
                    break;
                };
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            paths.lock().expect("HTTP collector paths").push(path);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
        }
    });
    collector
}
