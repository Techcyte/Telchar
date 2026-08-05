#![forbid(unsafe_code)]

use std::io::{self, Read, Write};

pub const CLIENT_WORKER_MAGIC: u64 = 0x6e69_7863;
pub const SERVER_WORKER_MAGIC: u64 = 0x6478_696f;
pub const MINIMUM_WORKER_VERSION: WorkerVersion = WorkerVersion::new(1, 18);
pub const LATEST_WORKER_VERSION: WorkerVersion = WorkerVersion::new(1, 38);
pub const FEATURE_NEGOTIATION_VERSION: WorkerVersion = WorkerVersion::new(1, 38);
pub const STDERR_LAST: u64 = 0x616c_7473;
const MAXIMUM_HANDSHAKE_FEATURES: usize = 64;
const MAXIMUM_HANDSHAKE_FEATURE_LENGTH: usize = 1024;

pub fn protocol_name() -> &'static str {
    "Nix worker protocol"
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct WorkerVersion {
    major: u8,
    minor: u8,
}

impl WorkerVersion {
    pub const fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }

    pub const fn to_wire(self) -> u64 {
        ((self.major as u64) << 8) | self.minor as u64
    }

    pub const fn from_wire(value: u64) -> Self {
        Self::new(((value & 0xff00) >> 8) as u8, (value & 0x00ff) as u8)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiatedWorkerVersion {
    pub version: WorkerVersion,
    pub features: Vec<String>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerOperation {
    IsValidPath,
    QueryReferrers,
    AddToStore,
    AddTextToStore,
    BuildPaths,
    EnsurePath,
    AddTempRoot,
    AddIndirectRoot,
    SyncWithGc,
    FindRoots,
    QueryDeriver,
    SetOptions,
    CollectGarbage,
    QuerySubstitutablePathInfo,
    QueryDerivationOutputs,
    QueryAllValidPaths,
    QueryPathInfo,
    QueryDerivationOutputNames,
    QueryPathFromHashPart,
    QuerySubstitutablePathInfos,
    QueryValidPaths,
    QuerySubstitutablePaths,
    QueryValidDerivers,
    OptimiseStore,
    VerifyStore,
    BuildDerivation,
    AddSignatures,
    NarFromPath,
    AddToStoreNar,
    QueryMissing,
    QueryDerivationOutputMap,
    RegisterDrvOutput,
    QueryRealisation,
    AddMultipleToStore,
    AddBuildLog,
    BuildPathsWithResults,
    AddPermRoot,
}

pub fn read_worker_operation(input: &mut &[u8]) -> Result<WorkerOperation, ProtocolError> {
    match read_worker_integer(input)? {
        1 => Ok(WorkerOperation::IsValidPath),
        6 => Ok(WorkerOperation::QueryReferrers),
        7 => Ok(WorkerOperation::AddToStore),
        8 => Ok(WorkerOperation::AddTextToStore),
        9 => Ok(WorkerOperation::BuildPaths),
        10 => Ok(WorkerOperation::EnsurePath),
        11 => Ok(WorkerOperation::AddTempRoot),
        12 => Ok(WorkerOperation::AddIndirectRoot),
        13 => Ok(WorkerOperation::SyncWithGc),
        14 => Ok(WorkerOperation::FindRoots),
        18 => Ok(WorkerOperation::QueryDeriver),
        19 => Ok(WorkerOperation::SetOptions),
        20 => Ok(WorkerOperation::CollectGarbage),
        21 => Ok(WorkerOperation::QuerySubstitutablePathInfo),
        22 => Ok(WorkerOperation::QueryDerivationOutputs),
        23 => Ok(WorkerOperation::QueryAllValidPaths),
        26 => Ok(WorkerOperation::QueryPathInfo),
        28 => Ok(WorkerOperation::QueryDerivationOutputNames),
        29 => Ok(WorkerOperation::QueryPathFromHashPart),
        30 => Ok(WorkerOperation::QuerySubstitutablePathInfos),
        31 => Ok(WorkerOperation::QueryValidPaths),
        32 => Ok(WorkerOperation::QuerySubstitutablePaths),
        33 => Ok(WorkerOperation::QueryValidDerivers),
        34 => Ok(WorkerOperation::OptimiseStore),
        35 => Ok(WorkerOperation::VerifyStore),
        36 => Ok(WorkerOperation::BuildDerivation),
        37 => Ok(WorkerOperation::AddSignatures),
        38 => Ok(WorkerOperation::NarFromPath),
        39 => Ok(WorkerOperation::AddToStoreNar),
        40 => Ok(WorkerOperation::QueryMissing),
        41 => Ok(WorkerOperation::QueryDerivationOutputMap),
        42 => Ok(WorkerOperation::RegisterDrvOutput),
        43 => Ok(WorkerOperation::QueryRealisation),
        44 => Ok(WorkerOperation::AddMultipleToStore),
        45 => Ok(WorkerOperation::AddBuildLog),
        46 => Ok(WorkerOperation::BuildPathsWithResults),
        47 => Ok(WorkerOperation::AddPermRoot),
        _ => Err(ProtocolError::UnsupportedOperation),
    }
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

pub fn read_client_worker_magic(input: &mut &[u8]) -> Result<(), ProtocolError> {
    if read_worker_integer(input)? == CLIENT_WORKER_MAGIC {
        Ok(())
    } else {
        Err(ProtocolError::VersionMismatch)
    }
}

pub fn negotiate_worker_version(
    client_version: WorkerVersion,
    client_features: &[String],
    server_features: &[String],
) -> Result<NegotiatedWorkerVersion, ProtocolError> {
    let version = client_version.min(LATEST_WORKER_VERSION);
    if version < MINIMUM_WORKER_VERSION {
        return Err(ProtocolError::VersionMismatch);
    }

    let features = if version >= WorkerVersion::new(1, 38) {
        client_features
            .iter()
            .filter(|feature| server_features.contains(feature))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    Ok(NegotiatedWorkerVersion { version, features })
}

pub fn perform_server_handshake<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    server_features: &[String],
) -> io::Result<NegotiatedWorkerVersion> {
    if read_worker_integer_from(input)? != CLIENT_WORKER_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker handshake magic mismatch",
        ));
    }

    write_worker_integer_to(output, SERVER_WORKER_MAGIC)?;
    write_worker_integer_to(output, LATEST_WORKER_VERSION.to_wire())?;
    output.flush()?;

    let client_version = WorkerVersion::from_wire(read_worker_integer_from(input)?);
    let client_features =
        if client_version.min(LATEST_WORKER_VERSION) >= FEATURE_NEGOTIATION_VERSION {
            read_worker_strings_from(input)?
        } else {
            Vec::new()
        };
    let negotiated = negotiate_worker_version(client_version, &client_features, server_features)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "unsupported worker version"))?;

    if negotiated.version >= FEATURE_NEGOTIATION_VERSION {
        write_worker_strings_to(output, server_features)?;
        output.flush()?;
    }

    Ok(negotiated)
}

pub fn complete_server_post_handshake<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    version: WorkerVersion,
    daemon_version: &str,
) -> io::Result<()> {
    if version >= WorkerVersion::new(1, 14) && read_worker_integer_from(input)? != 0 {
        read_worker_integer_from(input)?;
    }
    if version >= WorkerVersion::new(1, 11) {
        read_worker_integer_from(input)?;
    }
    if version >= WorkerVersion::new(1, 33) {
        write_worker_byte_string_to(output, daemon_version.as_bytes())?;
    }
    if version >= WorkerVersion::new(1, 35) {
        write_worker_integer_to(output, 0)?;
    }
    write_worker_integer_to(output, STDERR_LAST)?;
    output.flush()
}

fn read_worker_integer_from(input: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_worker_byte_string_from(
    input: &mut impl Read,
    maximum_length: usize,
) -> io::Result<Vec<u8>> {
    let length = usize::try_from(read_worker_integer_from(input)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "worker string exceeds limit"))?;
    if length > maximum_length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker string exceeds limit",
        ));
    }
    let padding_length = (8 - length % 8) % 8;
    let mut framed = vec![0; length + padding_length];
    input.read_exact(&mut framed)?;
    if framed[length..].iter().any(|byte| *byte != 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker string padding is not zero",
        ));
    }
    framed.truncate(length);
    Ok(framed)
}

fn read_worker_strings_from(input: &mut impl Read) -> io::Result<Vec<String>> {
    let count = usize::try_from(read_worker_integer_from(input)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "too many worker features"))?;
    if count > MAXIMUM_HANDSHAKE_FEATURES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "too many worker features",
        ));
    }
    (0..count)
        .map(|_| {
            let feature = read_worker_byte_string_from(input, MAXIMUM_HANDSHAKE_FEATURE_LENGTH)?;
            String::from_utf8(feature).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "worker feature is not UTF-8")
            })
        })
        .collect()
}

fn write_worker_integer_to(output: &mut impl Write, value: u64) -> io::Result<()> {
    output.write_all(&value.to_le_bytes())
}

fn write_worker_byte_string_to(output: &mut impl Write, value: &[u8]) -> io::Result<()> {
    write_worker_integer_to(output, value.len() as u64)?;
    output.write_all(value)?;
    output.write_all(&[0; 7][..(8 - value.len() % 8) % 8])
}

fn write_worker_strings_to(output: &mut impl Write, values: &[String]) -> io::Result<()> {
    write_worker_integer_to(output, values.len() as u64)?;
    for value in values {
        write_worker_byte_string_to(output, value.as_bytes())?;
    }
    Ok(())
}

pub fn write_worker_integer(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

pub fn write_server_worker_magic(output: &mut Vec<u8>) {
    write_worker_integer(output, SERVER_WORKER_MAGIC);
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
        CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, MINIMUM_WORKER_VERSION, ProtocolError,
        SERVER_WORKER_MAGIC, WorkerOperation, WorkerVersion, protocol_name,
        read_client_worker_magic, read_worker_byte_string, read_worker_integer,
        read_worker_operation, write_server_worker_magic, write_worker_byte_string,
        write_worker_integer,
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

    #[test]
    fn accepts_only_the_pinned_client_worker_magic() {
        let accepted_bytes = CLIENT_WORKER_MAGIC.to_le_bytes();
        let rejected_bytes = 0_u64.to_le_bytes();
        let mut accepted = accepted_bytes.as_slice();
        let mut rejected = rejected_bytes.as_slice();

        assert_eq!(read_client_worker_magic(&mut accepted), Ok(()));
        assert_eq!(
            read_client_worker_magic(&mut rejected),
            Err(ProtocolError::VersionMismatch)
        );
    }

    #[test]
    fn writes_the_pinned_server_worker_magic() {
        let mut output = Vec::new();

        write_server_worker_magic(&mut output);

        assert_eq!(SERVER_WORKER_MAGIC, 0x6478_696f);
        assert_eq!(output, b"oixd\0\0\0\0");
    }

    #[test]
    fn parses_pinned_worker_operation_codes_at_typed_boundaries() {
        let mut set_options = &19_u64.to_le_bytes()[..];
        let mut build_paths_with_results = &46_u64.to_le_bytes()[..];

        assert_eq!(
            read_worker_operation(&mut set_options),
            Ok(WorkerOperation::SetOptions)
        );
        assert_eq!(
            read_worker_operation(&mut build_paths_with_results),
            Ok(WorkerOperation::BuildPathsWithResults)
        );
    }

    #[test]
    fn negotiates_supported_worker_versions_and_features() {
        let client_features = vec!["one".to_owned(), "two".to_owned()];
        let server_features = vec!["two".to_owned(), "three".to_owned()];

        let below_minimum = super::negotiate_worker_version(WorkerVersion::new(1, 17), &[], &[]);
        let minimum = super::negotiate_worker_version(MINIMUM_WORKER_VERSION, &[], &[]).unwrap();
        let supported =
            super::negotiate_worker_version(WorkerVersion::new(1, 30), &[], &[]).unwrap();
        let maximum = super::negotiate_worker_version(
            LATEST_WORKER_VERSION,
            &client_features,
            &server_features,
        )
        .unwrap();
        let newer_same_major = super::negotiate_worker_version(
            WorkerVersion::new(1, 39),
            &client_features,
            &server_features,
        )
        .unwrap();
        let newer_major = super::negotiate_worker_version(
            WorkerVersion::new(2, 18),
            &client_features,
            &server_features,
        )
        .unwrap();

        assert_eq!(below_minimum, Err(ProtocolError::VersionMismatch));
        assert_eq!(minimum.version, MINIMUM_WORKER_VERSION);
        assert_eq!(supported.version, WorkerVersion::new(1, 30));
        assert_eq!(maximum.version, LATEST_WORKER_VERSION);
        assert_eq!(maximum.features, vec!["two"]);
        assert_eq!(newer_same_major.version, LATEST_WORKER_VERSION);
        assert_eq!(newer_major.version, LATEST_WORKER_VERSION);
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
