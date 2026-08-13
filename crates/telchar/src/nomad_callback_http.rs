use std::io::{self, Read, Write};

const TRANSFER_CONTENT_TYPE: &str = "application/vnd.telchar.nomad-transfer";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CallbackHttpLimits {
    maximum_header_bytes: usize,
    maximum_body_bytes: usize,
}

impl CallbackHttpLimits {
    pub const fn new(maximum_header_bytes: usize, maximum_body_bytes: usize) -> Self {
        Self {
            maximum_header_bytes,
            maximum_body_bytes,
        }
    }
}

pub fn handle_connection(
    stream: &mut (impl Read + Write),
    limits: CallbackHttpLimits,
    mut admit: impl FnMut(&str, &str, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let request = match read_request(stream, limits) {
        Ok(request) => request,
        Err(error) => {
            write_response(stream, "400 Bad Request")?;
            return Err(error);
        }
    };
    if let Err(error) = admit(request.method, request.path, &request.body) {
        write_response(stream, "403 Forbidden")?;
        return Err(io::Error::new(
            error.kind(),
            "Nomad callback admission failed",
        ));
    }
    write_response(stream, "204 No Content")
}

struct Request<'a> {
    method: &'a str,
    path: &'a str,
    body: Vec<u8>,
}

fn read_request(
    stream: &mut impl Read,
    limits: CallbackHttpLimits,
) -> io::Result<Request<'static>> {
    if limits.maximum_header_bytes == 0 || limits.maximum_body_bytes == 0 {
        return Err(invalid("Nomad callback HTTP limits are invalid"));
    }
    let mut headers = Vec::new();
    let mut byte = [0_u8; 1];
    while !headers.ends_with(b"\r\n\r\n") {
        if headers.len() >= limits.maximum_header_bytes {
            return Err(invalid("Nomad callback HTTP headers exceed limit"));
        }
        if stream.read(&mut byte)? != 1 {
            return Err(invalid("Nomad callback HTTP headers are incomplete"));
        }
        headers.push(byte[0]);
    }
    let headers = std::str::from_utf8(&headers)
        .map_err(|_| invalid("Nomad callback HTTP headers are invalid"))?;
    let mut lines = headers[..headers.len() - 4].split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| invalid("Nomad callback HTTP request line is invalid"))?;
    let mut request_parts = request_line.split(' ');
    let method = request_parts
        .next()
        .ok_or_else(|| invalid("Nomad callback HTTP request line is invalid"))?;
    let path = request_parts
        .next()
        .ok_or_else(|| invalid("Nomad callback HTTP request line is invalid"))?;
    let version = request_parts
        .next()
        .ok_or_else(|| invalid("Nomad callback HTTP request line is invalid"))?;
    if request_parts.next().is_some()
        || method != "POST"
        || path != "/callback"
        || version != "HTTP/1.1"
    {
        return Err(invalid("Nomad callback HTTP request line is invalid"));
    }

    let mut content_length = None;
    let mut content_type = None;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("Nomad callback HTTP header is invalid"))?;
        if name.is_empty() || value.is_empty() || !value.starts_with(' ') {
            return Err(invalid("Nomad callback HTTP header is invalid"));
        }
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(invalid("Nomad callback HTTP content length is invalid"));
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| invalid("Nomad callback HTTP content length is invalid"))?,
            );
        } else if name.eq_ignore_ascii_case("content-type") {
            if content_type.replace(value).is_some() {
                return Err(invalid("Nomad callback HTTP content type is invalid"));
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(invalid(
                "Nomad callback HTTP transfer encoding is unsupported",
            ));
        }
    }
    let content_length =
        content_length.ok_or_else(|| invalid("Nomad callback HTTP content length is missing"))?;
    if content_length == 0 || content_length > limits.maximum_body_bytes {
        return Err(invalid("Nomad callback HTTP body length is invalid"));
    }
    if content_type != Some(TRANSFER_CONTENT_TYPE) {
        return Err(invalid("Nomad callback HTTP content type is invalid"));
    }
    let mut body = vec![0; content_length];
    stream
        .read_exact(&mut body)
        .map_err(|_| invalid("Nomad callback HTTP body is incomplete"))?;
    Ok(Request {
        method: "POST",
        path: "/callback",
        body,
    })
}

fn write_response(stream: &mut impl Write, status: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
