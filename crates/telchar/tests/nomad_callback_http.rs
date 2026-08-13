use std::io::{Read, Write};

use telchar::nomad_callback_http::{handle_connection, CallbackHttpLimits};

struct FragmentedStream {
    input: Vec<u8>,
    offset: usize,
    output: Vec<u8>,
    maximum_read: usize,
}

impl FragmentedStream {
    fn new(input: impl Into<Vec<u8>>, maximum_read: usize) -> Self {
        Self {
            input: input.into(),
            offset: 0,
            output: Vec::new(),
            maximum_read,
        }
    }
}

impl Read for FragmentedStream {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let available = self.input.len().saturating_sub(self.offset);
        let length = available.min(output.len()).min(self.maximum_read);
        output[..length].copy_from_slice(&self.input[self.offset..self.offset + length]);
        self.offset += length;
        Ok(length)
    }
}

impl Write for FragmentedStream {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        self.output.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn admits_exact_bounded_callback_request() {
    let body = b"authentication-frame";
    let request = format!(
        "POST /callback HTTP/1.1\r\nHost: gateway\r\nContent-Type: application/vnd.telchar.nomad-transfer\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body),
    );
    let mut stream = FragmentedStream::new(request, 3);
    let mut admitted = Vec::new();

    handle_connection(
        &mut stream,
        CallbackHttpLimits::new(1024, 4096),
        |method, path, received| {
            admitted.push((method.to_owned(), path.to_owned(), received.to_vec()));
            Ok(())
        },
    )
    .expect("request handles");

    assert_eq!(
        admitted,
        vec![("POST".to_owned(), "/callback".to_owned(), body.to_vec())]
    );
    assert!(String::from_utf8(stream.output)
        .expect("response is UTF-8")
        .starts_with("HTTP/1.1 204 No Content\r\n"));
}

#[test]
fn rejects_wrong_method_type_length_and_oversized_headers_without_admission() {
    for request in [
        "GET /callback HTTP/1.1\r\nContent-Type: application/vnd.telchar.nomad-transfer\r\nContent-Length: 0\r\n\r\n".to_owned(),
        "POST /callback HTTP/1.1\r\nContent-Type: text/plain\r\nContent-Length: 0\r\n\r\n".to_owned(),
        "POST /callback HTTP/1.1\r\nContent-Type: application/vnd.telchar.nomad-transfer\r\nContent-Length: 9\r\n\r\nshort".to_owned(),
        format!("POST /callback HTTP/1.1\r\nX-Fill: {}\r\n\r\n", "a".repeat(1100)),
    ] {
        let mut stream = FragmentedStream::new(request, 11);
        let mut called = false;
        assert!(handle_connection(
            &mut stream,
            CallbackHttpLimits::new(1024, 8),
            |_, _, _| {
                called = true;
                Ok(())
            },
        )
        .is_err());
        assert!(!called);
        assert!(String::from_utf8(stream.output)
            .expect("response is UTF-8")
            .starts_with("HTTP/1.1 400 Bad Request\r\n"));
    }
}

#[test]
fn redacts_admission_failure_and_closes_connection() {
    let request = "POST /callback HTTP/1.1\r\nContent-Type: application/vnd.telchar.nomad-transfer\r\nContent-Length: 1\r\n\r\nx";
    let mut stream = FragmentedStream::new(request, 128);

    assert!(handle_connection(
        &mut stream,
        CallbackHttpLimits::new(1024, 8),
        |_, _, _| Err(std::io::Error::other("secret capability material")),
    )
    .is_err());
    let response = String::from_utf8(stream.output).expect("response is UTF-8");
    assert!(response.starts_with("HTTP/1.1 403 Forbidden\r\n"));
    assert!(!response.contains("secret"));
    assert!(response.contains("Connection: close\r\n"));
}
