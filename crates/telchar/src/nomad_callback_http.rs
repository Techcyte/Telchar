use std::io::{self, Read, Write};

use tungstenite::handshake::server::{Request, Response};
use tungstenite::http::{HeaderValue, StatusCode};
use tungstenite::protocol::{Message, WebSocket};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CallbackHttpLimits {
    maximum_header_bytes: usize,
    maximum_message_bytes: usize,
}

impl CallbackHttpLimits {
    pub fn new(maximum_header_bytes: usize, maximum_message_bytes: usize) -> Self {
        Self {
            maximum_header_bytes,
            maximum_message_bytes,
        }
    }
}

pub struct CallbackSocket<S> {
    inner: WebSocket<HeaderLimitedStream<S>>,
    maximum_message_bytes: usize,
}

pub fn accept_connection<S: Read + Write>(
    stream: S,
    limits: CallbackHttpLimits,
) -> io::Result<CallbackSocket<S>> {
    #[allow(clippy::result_large_err)]
    fn validate_upgrade(
        request: &Request,
        mut response: Response,
    ) -> Result<Response, tungstenite::handshake::server::ErrorResponse> {
        if request.method() != "GET"
            || request.uri().path().is_empty()
            || request.uri().query().is_some()
            || request.headers().get("sec-websocket-protocol")
                != Some(&HeaderValue::from_static("telchar-nomad-transfer-v1"))
        {
            let mut error = tungstenite::handshake::server::ErrorResponse::new(Some(
                "WebSocket request rejected".to_owned(),
            ));
            *error.status_mut() = StatusCode::BAD_REQUEST;
            return Err(error);
        }
        response.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("telchar-nomad-transfer-v1"),
        );
        Ok(response)
    }
    let stream = HeaderLimitedStream::new(stream, limits.maximum_header_bytes);
    let inner = tungstenite::accept_hdr(stream, validate_upgrade).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Nomad WebSocket handshake failed: {error:?}"),
        )
    })?;
    Ok(CallbackSocket {
        inner,
        maximum_message_bytes: limits.maximum_message_bytes,
    })
}

impl<S: Read + Write> CallbackSocket<S> {
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.inner.get_mut().inner
    }

    pub fn into_inner(self) -> S {
        self.inner.into_inner().inner
    }

    pub fn read_binary(&mut self) -> io::Result<Vec<u8>> {
        loop {
            match self.inner.read().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "Nomad WebSocket read failed")
            })? {
                Message::Binary(bytes) if bytes.len() <= self.maximum_message_bytes => {
                    return Ok(bytes.to_vec());
                }
                Message::Ping(payload) => self
                    .inner
                    .send(Message::Pong(payload))
                    .map_err(|_| io::Error::other("Nomad WebSocket write failed"))?,
                Message::Close(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Nomad WebSocket closed",
                    ));
                }
                Message::Binary(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Nomad WebSocket message exceeds limit",
                    ));
                }
                Message::Text(_) | Message::Pong(_) | Message::Frame(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Nomad WebSocket message is invalid",
                    ));
                }
            }
        }
    }

    pub fn write_binary(&mut self, message: Vec<u8>) -> io::Result<()> {
        if message.len() > self.maximum_message_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Nomad WebSocket message exceeds limit",
            ));
        }
        self.inner
            .send(Message::Binary(message.into()))
            .map_err(|_| io::Error::other("Nomad WebSocket write failed"))
    }
}

struct HeaderLimitedStream<S> {
    inner: S,
    maximum_header_bytes: usize,
    header_bytes: usize,
    header_complete: bool,
    suffix: [u8; 4],
    suffix_length: usize,
}

impl<S> HeaderLimitedStream<S> {
    fn new(inner: S, maximum_header_bytes: usize) -> Self {
        Self {
            inner,
            maximum_header_bytes,
            header_bytes: 0,
            header_complete: false,
            suffix: [0; 4],
            suffix_length: 0,
        }
    }
}

impl<S: Read> Read for HeaderLimitedStream<S> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.header_complete {
            return self.inner.read(output);
        }
        if self.header_bytes >= self.maximum_header_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Nomad WebSocket headers exceed limit",
            ));
        }
        let maximum = output
            .len()
            .min(self.maximum_header_bytes - self.header_bytes)
            .min(1024);
        let length = self.inner.read(&mut output[..maximum])?;
        for byte in &output[..length] {
            if self.suffix_length < self.suffix.len() {
                self.suffix[self.suffix_length] = *byte;
                self.suffix_length += 1;
            } else {
                self.suffix.rotate_left(1);
                self.suffix[3] = *byte;
            }
            if self.suffix_length == 4 && self.suffix == *b"\r\n\r\n" {
                self.header_complete = true;
            }
        }
        self.header_bytes += length;
        Ok(length)
    }
}

impl<S: Write> Write for HeaderLimitedStream<S> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.inner.write(input)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
