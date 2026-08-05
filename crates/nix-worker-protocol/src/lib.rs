#![forbid(unsafe_code)]

pub fn protocol_name() -> &'static str {
    "Nix worker protocol"
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    CleanEof,
    Truncated,
    SizeLimit,
    UnsupportedOperation,
    VersionMismatch,
    StoreFailure,
    InternalFailure,
}

pub fn read_worker_integer(input: &mut &[u8]) -> Result<u64, ProtocolError> {
    if input.is_empty() {
        return Err(ProtocolError::CleanEof);
    }

    if input.len() < 8 {
        return Err(ProtocolError::Truncated);
    }

    let (encoded, remaining) = input.split_at(8);
    *input = remaining;
    let mut bytes = [0; 8];
    bytes.copy_from_slice(encoded);
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::{ProtocolError, protocol_name, read_worker_integer};

    #[test]
    fn reports_protocol_name() {
        assert_eq!(protocol_name(), "Nix worker protocol");
    }

    #[test]
    fn distinguishes_protocol_failure_classes() {
        let cases = [
            ProtocolError::CleanEof,
            ProtocolError::Truncated,
            ProtocolError::SizeLimit,
            ProtocolError::UnsupportedOperation,
            ProtocolError::VersionMismatch,
            ProtocolError::StoreFailure,
            ProtocolError::InternalFailure,
        ];

        assert_eq!(cases.len(), 7);
    }

    #[test]
    fn reads_little_endian_worker_integers() {
        let mut zero = &b"\0\0\0\0\0\0\0\0"[..];
        let mut maximum = &b"\xff\xff\xff\xff\xff\xff\xff\xff"[..];
        let mut ordinary = &b"\x08\x07\x06\x05\x04\x03\x02\x01"[..];

        assert_eq!(read_worker_integer(&mut zero), Ok(0));
        assert_eq!(read_worker_integer(&mut maximum), Ok(u64::MAX));
        assert_eq!(read_worker_integer(&mut ordinary), Ok(0x0102_0304_0506_0708));
    }

    #[test]
    fn rejects_truncated_worker_integers() {
        let mut empty = &b""[..];
        let mut partial = &b"\0\0\0\0\0\0\0"[..];

        assert_eq!(read_worker_integer(&mut empty), Err(ProtocolError::CleanEof));
        assert_eq!(read_worker_integer(&mut partial), Err(ProtocolError::Truncated));
    }
}
