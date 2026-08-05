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

pub fn write_worker_integer(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

pub fn read_worker_byte_string(
    input: &mut &[u8],
    maximum_length: usize,
) -> Result<Vec<u8>, ProtocolError> {
    let length = read_worker_integer(input)?;
    let length = usize::try_from(length).map_err(|_| ProtocolError::SizeLimit)?;
    if length > maximum_length {
        return Err(ProtocolError::SizeLimit);
    }

    let padding_length = (8 - length % 8) % 8;
    let framed_length = length
        .checked_add(padding_length)
        .ok_or(ProtocolError::SizeLimit)?;
    if input.len() < framed_length {
        return Err(ProtocolError::Truncated);
    }

    let (framed, remaining) = input.split_at(framed_length);
    let (payload, padding) = framed.split_at(length);
    if padding.iter().any(|byte| *byte != 0) {
        return Err(ProtocolError::InternalFailure);
    }

    *input = remaining;
    Ok(payload.to_vec())
}

pub fn write_worker_byte_string(output: &mut Vec<u8>, value: &[u8]) {
    write_worker_integer(output, value.len() as u64);
    output.extend_from_slice(value);
    output.resize(output.len() + (8 - value.len() % 8) % 8, 0);
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;

    use super::{
        ProtocolError, protocol_name, read_worker_byte_string, read_worker_integer,
        write_worker_byte_string, write_worker_integer,
    };

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
        assert_eq!(
            read_worker_integer(&mut ordinary),
            Ok(0x0102_0304_0506_0708)
        );
    }

    #[test]
    fn rejects_truncated_worker_integers() {
        let mut empty = &b""[..];
        let mut partial = &b"\0\0\0\0\0\0\0"[..];

        assert_eq!(
            read_worker_integer(&mut empty),
            Err(ProtocolError::CleanEof)
        );
        assert_eq!(
            read_worker_integer(&mut partial),
            Err(ProtocolError::Truncated)
        );
    }

    #[test]
    fn reads_bounded_worker_byte_strings() {
        let mut empty = &b"\0\0\0\0\0\0\0\0"[..];
        let mut ordinary = &b"\x03\0\0\0\0\0\0\0abc\0\0\0\0\0"[..];
        let mut padded = &b"\x09\0\0\0\0\0\0\0abcdefghi\0\0\0\0\0\0\0"[..];

        assert_eq!(read_worker_byte_string(&mut empty, 9), Ok(Vec::new()));
        assert_eq!(
            read_worker_byte_string(&mut ordinary, 9),
            Ok(b"abc".to_vec())
        );
        assert_eq!(
            read_worker_byte_string(&mut padded, 9),
            Ok(b"abcdefghi".to_vec())
        );
        assert!(empty.is_empty());
        assert!(ordinary.is_empty());
        assert!(padded.is_empty());
    }

    #[test]
    fn rejects_oversized_worker_byte_strings_before_allocation() {
        let mut input = &b"\x05\0\0\0\0\0\0\0"[..];

        assert_eq!(
            read_worker_byte_string(&mut input, 4),
            Err(ProtocolError::SizeLimit)
        );
        assert!(input.is_empty());
    }

    #[test]
    fn rejects_truncated_worker_byte_string_payload_or_padding() {
        let mut payload = &b"\x03\0\0\0\0\0\0\0ab"[..];
        let mut padding = &b"\x03\0\0\0\0\0\0\0abc\0\0\0\0"[..];

        assert_eq!(
            read_worker_byte_string(&mut payload, 3),
            Err(ProtocolError::Truncated)
        );
        assert_eq!(
            read_worker_byte_string(&mut padding, 3),
            Err(ProtocolError::Truncated)
        );
    }

    #[test]
    fn writes_worker_primitives_matching_golden_bytes() {
        let mut integer = Vec::new();
        let mut empty = Vec::new();
        let mut ordinary = Vec::new();
        let mut padded = Vec::new();

        write_worker_integer(&mut integer, 0x0102_0304_0506_0708);
        write_worker_byte_string(&mut empty, b"");
        write_worker_byte_string(&mut ordinary, b"abc");
        write_worker_byte_string(&mut padded, b"abcdefghi");

        assert_eq!(integer, b"\x08\x07\x06\x05\x04\x03\x02\x01");
        assert_eq!(empty, b"\0\0\0\0\0\0\0\0");
        assert_eq!(ordinary, b"\x03\0\0\0\0\0\0\0abc\0\0\0\0\0");
        assert_eq!(padded, b"\x09\0\0\0\0\0\0\0abcdefghi\0\0\0\0\0\0\0");
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, rng_seed: RngSeed::Fixed(0x5445_4c43_4841_5230), .. ProptestConfig::default() })]

        #[test]
        fn primitive_parsers_never_panic_and_respect_limits(input in proptest::collection::vec(any::<u8>(), 0..1024), maximum_length in 0usize..128) {
            let mut integer_input = input.as_slice();
            let mut byte_string_input = input.as_slice();

            let _ = read_worker_integer(&mut integer_input);
            let result = read_worker_byte_string(&mut byte_string_input, maximum_length);

            if let Ok(value) = result {
                prop_assert!(value.len() <= maximum_length);
            }
        }
    }
}
