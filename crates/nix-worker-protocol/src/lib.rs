#![forbid(unsafe_code)]

use std::io::{self, Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

pub const CLIENT_WORKER_MAGIC: u64 = 0x6e69_7863;
pub const SERVER_WORKER_MAGIC: u64 = 0x6478_696f;
pub const MINIMUM_WORKER_VERSION: WorkerVersion = WorkerVersion::new(1, 18);
pub const LATEST_WORKER_VERSION: WorkerVersion = WorkerVersion::new(1, 38);
pub const FEATURE_NEGOTIATION_VERSION: WorkerVersion = WorkerVersion::new(1, 38);
pub const STDERR_NEXT: u64 = 0x6f6c_6d67;
pub const STDERR_LAST: u64 = 0x616c_7473;
pub const STDERR_ERROR: u64 = 0x6378_7470;
pub const STDERR_START_ACTIVITY: u64 = 0x5354_5254;
pub const STDERR_STOP_ACTIVITY: u64 = 0x5354_4f50;
pub const STDERR_RESULT: u64 = 0x5253_4c54;
const MAXIMUM_HANDSHAKE_FEATURES: usize = 64;
const MAXIMUM_HANDSHAKE_FEATURE_LENGTH: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolSessionLimits {
    pub maximum_retained_metadata_bytes: usize,
    pub incomplete_message_idle_timeout: Duration,
}

impl ProtocolSessionLimits {
    pub const DEFAULT: Self = Self::new(16 * 1024 * 1024, Duration::from_secs(30));

    pub const fn new(
        maximum_retained_metadata_bytes: usize,
        incomplete_message_idle_timeout: Duration,
    ) -> Self {
        Self {
            maximum_retained_metadata_bytes,
            incomplete_message_idle_timeout,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionAllocationBudget {
    retained_bytes: Arc<AtomicUsize>,
    maximum_retained_bytes: usize,
}

impl SessionAllocationBudget {
    pub fn new(limits: ProtocolSessionLimits) -> Self {
        Self {
            retained_bytes: Arc::new(AtomicUsize::new(0)),
            maximum_retained_bytes: limits.maximum_retained_metadata_bytes,
        }
    }

    pub fn charge(&self, bytes: usize) -> Result<SessionAllocationCharge, ProtocolError> {
        let mut retained_bytes = self.retained_bytes.load(Ordering::Acquire);
        loop {
            let updated_bytes = retained_bytes
                .checked_add(bytes)
                .filter(|updated_bytes| *updated_bytes <= self.maximum_retained_bytes)
                .ok_or(ProtocolError::SizeLimit)?;
            match self.retained_bytes.compare_exchange_weak(
                retained_bytes,
                updated_bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(SessionAllocationCharge {
                        retained_bytes: Arc::clone(&self.retained_bytes),
                        bytes,
                    });
                }
                Err(actual_bytes) => retained_bytes = actual_bytes,
            }
        }
    }

    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub struct SessionAllocationCharge {
    retained_bytes: Arc<AtomicUsize>,
    bytes: usize,
}

impl Drop for SessionAllocationCharge {
    fn drop(&mut self) {
        self.retained_bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

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

#[derive(Debug)]
pub struct NegotiatedWorkerVersion {
    pub version: WorkerVersion,
    pub features: Vec<String>,
    _feature_charge: Option<SessionAllocationCharges>,
}

impl PartialEq for NegotiatedWorkerVersion {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version && self.features == other.features
    }
}

impl Eq for NegotiatedWorkerVersion {}

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

impl WorkerOperation {
    pub const fn is_fixture_allowed(self) -> bool {
        matches!(
            self,
            Self::IsValidPath
                | Self::AddToStore
                | Self::AddTempRoot
                | Self::SetOptions
                | Self::QueryPathInfo
                | Self::QueryMissing
                | Self::BuildPathsWithResults
        )
    }
}

pub fn read_worker_operation(input: &mut &[u8]) -> Result<WorkerOperation, ProtocolError> {
    worker_operation_from_code(read_worker_integer(input)?)
}

pub fn worker_operation_from_code(code: u64) -> Result<WorkerOperation, ProtocolError> {
    match code {
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

pub fn read_worker_operation_from(input: &mut impl Read) -> io::Result<WorkerOperation> {
    worker_operation_from_code(read_worker_integer_from(input)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "unknown worker operation"))
}

pub fn write_worker_error(output: &mut impl Write, message: &str) -> io::Result<()> {
    write_worker_integer_to(output, STDERR_ERROR)?;
    write_worker_byte_string_to(output, b"Error")?;
    write_worker_integer_to(output, 0)?;
    write_worker_byte_string_to(output, b"Error")?;
    write_worker_byte_string_to(output, message.as_bytes())?;
    write_worker_integer_to(output, 0)?;
    write_worker_integer_to(output, 0)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureSetOptions {
    pub override_count: u64,
    pub string_lengths: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureStorePathRequest {
    pub operation: WorkerOperation,
    pub path_length: u64,
}

pub fn read_fixture_store_path_request(
    input: &mut &[u8],
    expected_operation: WorkerOperation,
) -> Result<FixtureStorePathRequest, ProtocolError> {
    match expected_operation {
        WorkerOperation::AddTempRoot | WorkerOperation::IsValidPath => {}
        _ => return Err(ProtocolError::UnsupportedOperation),
    }
    let operation = read_worker_operation(input)?;
    if operation != expected_operation {
        return Err(ProtocolError::UnsupportedOperation);
    }
    Ok(FixtureStorePathRequest {
        operation,
        path_length: read_fixture_string_length(input, 153)?,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureAddToStoreRequest {
    pub name_length: u64,
    pub content_address_length: u64,
    pub reference_count: u64,
    pub upload_chunk_lengths: Vec<u64>,
    pub upload_length: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureDerivedPathsRequest {
    pub operation: WorkerOperation,
    pub path_count: u64,
    pub path_lengths: Vec<u64>,
    pub build_mode: Option<u64>,
}

pub fn read_fixture_derived_paths_request(
    input: &mut &[u8],
    expected_operation: WorkerOperation,
) -> Result<FixtureDerivedPathsRequest, ProtocolError> {
    match expected_operation {
        WorkerOperation::QueryMissing | WorkerOperation::BuildPathsWithResults => {}
        _ => return Err(ProtocolError::UnsupportedOperation),
    }
    let operation = read_worker_operation(input)?;
    if operation != expected_operation {
        return Err(ProtocolError::UnsupportedOperation);
    }
    let path_count = read_fixture_count(input, 1)?;
    let path_lengths = (0..path_count)
        .map(|_| read_fixture_string_length(input, 157))
        .collect::<Result<Vec<_>, _>>()?;
    let build_mode = if operation == WorkerOperation::BuildPathsWithResults {
        let mode = read_worker_integer(input)?;
        if mode > 2 {
            return Err(ProtocolError::InternalFailure);
        }
        Some(mode)
    } else {
        None
    };
    Ok(FixtureDerivedPathsRequest {
        operation,
        path_count,
        path_lengths,
        build_mode,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureQueryMissingResponse {
    pub will_build_count: u64,
    pub will_substitute_count: u64,
    pub unknown_count: u64,
    pub path_lengths: Vec<u64>,
}

pub fn read_fixture_query_missing_response(
    input: &mut &[u8],
) -> Result<FixtureQueryMissingResponse, ProtocolError> {
    let will_build_count = read_fixture_count(input, 1)?;
    let mut path_lengths = Vec::with_capacity(will_build_count as usize);
    for _ in 0..will_build_count {
        path_lengths.push(read_fixture_string_length(input, 153)?);
    }
    let will_substitute_count = read_fixture_count(input, 0)?;
    let unknown_count = read_fixture_count(input, 0)?;
    read_worker_integer(input)?;
    read_worker_integer(input)?;
    Ok(FixtureQueryMissingResponse {
        will_build_count,
        will_substitute_count,
        unknown_count,
        path_lengths,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureUnkeyedPathInfo {
    pub deriver_length: u64,
    pub nar_hash_length: u64,
    pub reference_count: u64,
    pub signature_count: u64,
    pub content_address_length: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureValidPathInfo {
    pub path_length: u64,
    pub deriver_length: u64,
    pub nar_hash_length: u64,
    pub reference_count: u64,
    pub signature_count: u64,
    pub content_address_length: u64,
}

pub fn read_fixture_valid_path_info(
    input: &mut &[u8],
) -> Result<FixtureValidPathInfo, ProtocolError> {
    let path_length = read_fixture_string_length(input, 153)?;
    let info = read_fixture_unkeyed_path_info(input)?;
    Ok(FixtureValidPathInfo {
        path_length,
        deriver_length: info.deriver_length,
        nar_hash_length: info.nar_hash_length,
        reference_count: info.reference_count,
        signature_count: info.signature_count,
        content_address_length: info.content_address_length,
    })
}

pub fn read_fixture_query_path_info_response(
    input: &mut &[u8],
) -> Result<Option<FixtureUnkeyedPathInfo>, ProtocolError> {
    if read_fixture_boolean(input)? {
        Ok(Some(read_fixture_unkeyed_path_info(input)?))
    } else {
        Ok(None)
    }
}

fn read_fixture_unkeyed_path_info(
    input: &mut &[u8],
) -> Result<FixtureUnkeyedPathInfo, ProtocolError> {
    let deriver_length = read_fixture_string_length(input, 0)?;
    let nar_hash_length = read_fixture_string_length(input, 64)?;
    let reference_count = read_fixture_count(input, 0)?;
    read_worker_integer(input)?;
    read_worker_integer(input)?;
    read_fixture_boolean(input)?;
    let signature_count = read_fixture_count(input, 0)?;
    let content_address_length = read_fixture_string_length(input, 64)?;
    Ok(FixtureUnkeyedPathInfo {
        deriver_length,
        nar_hash_length,
        reference_count,
        signature_count,
        content_address_length,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureBuildPathsWithResultsResponse {
    pub result_count: u64,
    pub path_length: u64,
    pub status: u64,
    pub error_length: u64,
    pub output_count: u64,
    pub output_id_length: u64,
    pub output_realisation_length: u64,
}

pub fn read_fixture_build_paths_with_results_response(
    input: &mut &[u8],
) -> Result<FixtureBuildPathsWithResultsResponse, ProtocolError> {
    let result_count = read_fixture_count(input, 1)?;
    if result_count == 0 {
        return Err(ProtocolError::InternalFailure);
    }
    let path_length = read_fixture_string_length(input, 157)?;
    let status = read_worker_integer(input)?;
    if status > 14 {
        return Err(ProtocolError::InternalFailure);
    }
    let error_length = read_fixture_string_length(input, 0)?;
    read_worker_integer(input)?;
    read_fixture_boolean(input)?;
    read_worker_integer(input)?;
    read_worker_integer(input)?;
    read_fixture_optional_duration(input)?;
    read_fixture_optional_duration(input)?;
    let output_count = read_fixture_count(input, 1)?;
    if output_count == 0 {
        return Err(ProtocolError::InternalFailure);
    }
    let output_id_length = read_fixture_string_length(input, 75)?;
    let output_realisation_length = read_fixture_string_length(input, 196)?;
    Ok(FixtureBuildPathsWithResultsResponse {
        result_count,
        path_length,
        status,
        error_length,
        output_count,
        output_id_length,
        output_realisation_length,
    })
}

fn read_fixture_optional_duration(input: &mut &[u8]) -> Result<(), ProtocolError> {
    match read_worker_integer(input)? {
        0 => Ok(()),
        1 => {
            read_worker_integer(input)?;
            Ok(())
        }
        _ => Err(ProtocolError::InternalFailure),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FixtureStderrFrame {
    Next {
        message_length: u64,
    },
    StartActivity {
        message_length: u64,
        field_count: u64,
        field_string_lengths: Vec<u64>,
    },
    StopActivity,
    Result {
        field_count: u64,
        field_string_lengths: Vec<u64>,
    },
    Last,
}

pub fn read_fixture_stderr_frame(input: &mut &[u8]) -> Result<FixtureStderrFrame, ProtocolError> {
    match read_worker_integer(input)? {
        STDERR_NEXT => Ok(FixtureStderrFrame::Next {
            message_length: read_fixture_string_length(input, 145)?,
        }),
        STDERR_START_ACTIVITY => {
            read_worker_integer(input)?;
            read_worker_integer(input)?;
            read_worker_integer(input)?;
            let message_length = read_fixture_string_length(input, 164)?;
            let (field_count, field_string_lengths) = read_fixture_activity_fields(input)?;
            read_worker_integer(input)?;
            Ok(FixtureStderrFrame::StartActivity {
                message_length,
                field_count,
                field_string_lengths,
            })
        }
        STDERR_STOP_ACTIVITY => {
            read_worker_integer(input)?;
            Ok(FixtureStderrFrame::StopActivity)
        }
        STDERR_RESULT => {
            read_worker_integer(input)?;
            read_worker_integer(input)?;
            let (field_count, field_string_lengths) = read_fixture_activity_fields(input)?;
            Ok(FixtureStderrFrame::Result {
                field_count,
                field_string_lengths,
            })
        }
        STDERR_LAST => Ok(FixtureStderrFrame::Last),
        _ => Err(ProtocolError::UnsupportedOperation),
    }
}

fn read_fixture_activity_fields(input: &mut &[u8]) -> Result<(u64, Vec<u64>), ProtocolError> {
    let field_count = read_fixture_count(input, 4)?;
    let mut field_string_lengths = Vec::new();
    for _ in 0..field_count {
        match read_worker_integer(input)? {
            0 => {
                read_worker_integer(input)?;
            }
            1 => field_string_lengths.push(read_fixture_string_length(input, 153)?),
            _ => return Err(ProtocolError::InternalFailure),
        }
    }
    Ok((field_count, field_string_lengths))
}

pub fn read_fixture_add_to_store_request(
    input: &mut &[u8],
) -> Result<FixtureAddToStoreRequest, ProtocolError> {
    if read_worker_operation(input)? != WorkerOperation::AddToStore {
        return Err(ProtocolError::UnsupportedOperation);
    }
    let name_length = read_fixture_string_length(input, 27)?;
    let content_address_length = read_fixture_string_length(input, 11)?;
    let reference_count = read_fixture_count(input, 0)?;
    for _ in 0..reference_count {
        read_fixture_string_length(input, 153)?;
    }
    read_fixture_boolean(input)?;

    let mut upload_chunk_lengths = Vec::new();
    let mut upload_length = 0_u64;
    loop {
        let length = read_worker_integer(input)?;
        if length == 0 {
            break;
        }
        if !upload_chunk_lengths.is_empty() || length > 502 {
            return Err(ProtocolError::SizeLimit);
        }
        upload_length = upload_length
            .checked_add(length)
            .filter(|length| *length <= 502)
            .ok_or(ProtocolError::SizeLimit)?;
        let length = usize::try_from(length).map_err(|_| ProtocolError::SizeLimit)?;
        if input.len() < length {
            return Err(ProtocolError::Truncated);
        }
        let (_, remaining) = input.split_at(length);
        *input = remaining;
        upload_chunk_lengths.push(length as u64);
    }
    Ok(FixtureAddToStoreRequest {
        name_length,
        content_address_length,
        reference_count,
        upload_chunk_lengths,
        upload_length,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureHandshake {
    pub version: WorkerVersion,
    pub feature_lengths: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureServerHandshakeInfo {
    pub daemon_version_length: Option<u64>,
    pub trust_status: Option<u64>,
}

pub fn read_fixture_client_handshake(input: &mut &[u8]) -> Result<FixtureHandshake, ProtocolError> {
    if read_worker_integer(input)? != CLIENT_WORKER_MAGIC {
        return Err(ProtocolError::VersionMismatch);
    }
    read_fixture_handshake_version(input)
}

pub fn read_fixture_server_handshake(input: &mut &[u8]) -> Result<FixtureHandshake, ProtocolError> {
    if read_worker_integer(input)? != SERVER_WORKER_MAGIC {
        return Err(ProtocolError::VersionMismatch);
    }
    read_fixture_handshake_version(input)
}

fn read_fixture_handshake_version(input: &mut &[u8]) -> Result<FixtureHandshake, ProtocolError> {
    let version = WorkerVersion::from_wire(read_worker_integer(input)?);
    let feature_lengths = if version >= FEATURE_NEGOTIATION_VERSION {
        read_fixture_string_lengths(input, 64, 1_024)?
    } else {
        Vec::new()
    };
    Ok(FixtureHandshake {
        version,
        feature_lengths,
    })
}

pub fn read_fixture_client_post_handshake(
    input: &mut &[u8],
    version: WorkerVersion,
) -> Result<(), ProtocolError> {
    if version >= WorkerVersion::new(1, 14) && read_worker_integer(input)? != 0 {
        read_worker_integer(input)?;
    }
    if version >= WorkerVersion::new(1, 11) {
        read_worker_integer(input)?;
    }
    Ok(())
}

pub fn read_fixture_server_handshake_info(
    input: &mut &[u8],
    version: WorkerVersion,
) -> Result<FixtureServerHandshakeInfo, ProtocolError> {
    let daemon_version_length = if version >= WorkerVersion::new(1, 33) {
        Some(read_fixture_string_length(input, 1_024)?)
    } else {
        None
    };
    let trust_status = if version >= WorkerVersion::new(1, 35) {
        let status = read_worker_integer(input)?;
        if status > 2 {
            return Err(ProtocolError::InternalFailure);
        }
        Some(status)
    } else {
        None
    };
    Ok(FixtureServerHandshakeInfo {
        daemon_version_length,
        trust_status,
    })
}

pub fn read_fixture_terminal_frame(input: &mut &[u8]) -> Result<(), ProtocolError> {
    if read_worker_integer(input)? == STDERR_LAST {
        Ok(())
    } else {
        Err(ProtocolError::UnsupportedOperation)
    }
}

pub fn read_fixture_set_options(input: &mut &[u8]) -> Result<FixtureSetOptions, ProtocolError> {
    if read_worker_operation(input)? != WorkerOperation::SetOptions {
        return Err(ProtocolError::UnsupportedOperation);
    }

    for _ in 0..12 {
        read_worker_integer(input)?;
    }

    let override_count = read_worker_integer(input)?;
    if override_count > 256 {
        return Err(ProtocolError::SizeLimit);
    }

    let mut string_lengths = Vec::with_capacity((override_count * 2) as usize);
    for _ in 0..override_count {
        for _ in 0..2 {
            string_lengths.push(read_fixture_string_length(input, 16_384)?);
        }
    }

    Ok(FixtureSetOptions {
        override_count,
        string_lengths,
    })
}

fn read_fixture_string_lengths(
    input: &mut &[u8],
    maximum_count: u64,
    maximum_length: u64,
) -> Result<Vec<u64>, ProtocolError> {
    let count = read_fixture_count(input, maximum_count)?;
    (0..count)
        .map(|_| read_fixture_string_length(input, maximum_length))
        .collect()
}

fn read_fixture_count(input: &mut &[u8], maximum: u64) -> Result<u64, ProtocolError> {
    let count = read_worker_integer(input)?;
    if count > maximum {
        return Err(ProtocolError::SizeLimit);
    }
    Ok(count)
}

fn read_fixture_boolean(input: &mut &[u8]) -> Result<bool, ProtocolError> {
    match read_worker_integer(input)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ProtocolError::InternalFailure),
    }
}

fn read_fixture_string_length(
    input: &mut &[u8],
    maximum_length: u64,
) -> Result<u64, ProtocolError> {
    let length = read_worker_integer(input)?;
    if length > maximum_length {
        return Err(ProtocolError::SizeLimit);
    }
    let length = usize::try_from(length).map_err(|_| ProtocolError::SizeLimit)?;
    let padding_length = (8 - length % 8) % 8;
    let framed_length = length
        .checked_add(padding_length)
        .ok_or(ProtocolError::SizeLimit)?;
    if input.len() < framed_length {
        return Err(ProtocolError::Truncated);
    }
    let (framed, remaining) = input.split_at(framed_length);
    if framed[length..].iter().any(|byte| *byte != 0) {
        return Err(ProtocolError::InternalFailure);
    }
    *input = remaining;
    Ok(length as u64)
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

    Ok(NegotiatedWorkerVersion {
        version,
        features,
        _feature_charge: None,
    })
}

pub trait WorkerInput: Read {
    fn complete_message(&mut self) {}
}

impl WorkerInput for &[u8] {}

impl<R: WorkerInput + ?Sized> WorkerInput for &mut R {
    fn complete_message(&mut self) {
        (**self).complete_message();
    }
}

pub struct WorkerReader<R> {
    input: R,
    budget: SessionAllocationBudget,
}

impl<R: WorkerInput> WorkerReader<R> {
    pub fn new(input: R, limits: ProtocolSessionLimits) -> Self {
        Self {
            input,
            budget: SessionAllocationBudget::new(limits),
        }
    }

    pub fn retained_metadata_bytes(&self) -> usize {
        self.budget.retained_bytes()
    }

    pub fn perform_server_handshake<W: Write>(
        &mut self,
        output: &mut W,
        server_features: &[String],
    ) -> io::Result<NegotiatedWorkerVersion> {
        if self.read_integer()? != CLIENT_WORKER_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "worker handshake magic mismatch",
            ));
        }

        write_worker_integer_to(output, SERVER_WORKER_MAGIC)?;
        write_worker_integer_to(output, LATEST_WORKER_VERSION.to_wire())?;
        output.flush()?;

        let client_version = WorkerVersion::from_wire(self.read_integer()?);
        let client_features =
            if client_version.min(LATEST_WORKER_VERSION) >= FEATURE_NEGOTIATION_VERSION {
                Some(self.read_strings()?)
            } else {
                None
            };
        let negotiated = negotiate_worker_version_with_budget(
            client_version,
            client_features
                .as_ref()
                .map(|features| features.values.as_slice())
                .unwrap_or_default(),
            server_features,
            &self.budget,
        )
        .map_err(|error| match error {
            ProtocolError::SizeLimit => io::Error::new(
                io::ErrorKind::InvalidData,
                "worker metadata exceeds session limit",
            ),
            _ => io::Error::new(io::ErrorKind::InvalidData, "unsupported worker version"),
        })?;
        drop(client_features);

        if negotiated.version >= FEATURE_NEGOTIATION_VERSION {
            write_worker_strings_to(output, server_features)?;
            output.flush()?;
        }

        self.input.complete_message();
        Ok(negotiated)
    }

    pub fn complete_server_post_handshake<W: Write>(
        &mut self,
        output: &mut W,
        version: WorkerVersion,
        daemon_version: &str,
    ) -> io::Result<()> {
        if version >= WorkerVersion::new(1, 14) && self.read_integer()? != 0 {
            self.read_integer()?;
        }
        if version >= WorkerVersion::new(1, 11) {
            self.read_integer()?;
        }
        if version >= WorkerVersion::new(1, 33) {
            write_worker_byte_string_to(output, daemon_version.as_bytes())?;
        }
        if version >= WorkerVersion::new(1, 35) {
            write_worker_integer_to(output, 0)?;
        }
        write_worker_integer_to(output, STDERR_LAST)?;
        output.flush()?;
        self.input.complete_message();
        Ok(())
    }

    pub fn read_operation(&mut self) -> io::Result<WorkerOperation> {
        let operation = worker_operation_from_code(self.read_integer()?)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "unknown worker operation"))?;
        self.input.complete_message();
        Ok(operation)
    }

    pub fn into_inner(self) -> R {
        self.input
    }

    fn read_integer(&mut self) -> io::Result<u64> {
        read_worker_integer_from(&mut self.input)
    }

    fn read_strings(&mut self) -> io::Result<DecodedWorkerStrings> {
        read_worker_strings_from(&mut self.input, &self.budget)
    }
}

fn read_worker_integer_from(input: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn negotiate_worker_version_with_budget(
    client_version: WorkerVersion,
    client_features: &[String],
    server_features: &[String],
    budget: &SessionAllocationBudget,
) -> Result<NegotiatedWorkerVersion, ProtocolError> {
    let version = client_version.min(LATEST_WORKER_VERSION);
    if version < MINIMUM_WORKER_VERSION {
        return Err(ProtocolError::VersionMismatch);
    }

    let feature_count = client_features
        .iter()
        .filter(|feature| server_features.contains(feature))
        .count();
    let feature_capacity = feature_count
        .checked_mul(std::mem::size_of::<String>())
        .ok_or(ProtocolError::SizeLimit)?;
    let feature_charge = budget.charge(feature_capacity)?;
    let mut features = Vec::with_capacity(feature_count);
    let mut feature_charges = Vec::with_capacity(feature_count);
    for feature in client_features
        .iter()
        .filter(|feature| server_features.contains(feature))
    {
        let charge = budget.charge(feature.capacity())?;
        features.push(feature.clone());
        feature_charges.push(charge);
    }
    let metadata_charge = SessionAllocationCharges {
        _collection_charge: feature_charge,
        _value_charges: feature_charges,
    };
    Ok(NegotiatedWorkerVersion {
        version,
        features,
        _feature_charge: Some(metadata_charge),
    })
}

#[derive(Debug)]
struct SessionAllocationCharges {
    _collection_charge: SessionAllocationCharge,
    _value_charges: Vec<SessionAllocationCharge>,
}

#[derive(Debug)]
struct DecodedWorkerStrings {
    values: Vec<String>,
    _collection_charge: SessionAllocationCharge,
    _value_charges: Vec<SessionAllocationCharge>,
}

fn read_worker_strings_from(
    input: &mut impl Read,
    budget: &SessionAllocationBudget,
) -> io::Result<DecodedWorkerStrings> {
    let count = usize::try_from(read_worker_integer_from(input)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "too many worker features"))?;
    if count > MAXIMUM_HANDSHAKE_FEATURES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "too many worker features",
        ));
    }
    let collection_capacity = count
        .checked_mul(std::mem::size_of::<String>())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "worker metadata exceeds session limit",
            )
        })?;
    let collection_charge = budget.charge(collection_capacity).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "worker metadata exceeds session limit",
        )
    })?;
    let mut values = Vec::with_capacity(count);
    let mut value_charges = Vec::with_capacity(count);
    for _ in 0..count {
        let (feature, charge) = read_worker_byte_string_with_charge_from(
            input,
            MAXIMUM_HANDSHAKE_FEATURE_LENGTH,
            budget,
        )?;
        let feature = String::from_utf8(feature).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "worker feature is not UTF-8")
        })?;
        values.push(feature);
        value_charges.push(charge);
    }
    Ok(DecodedWorkerStrings {
        values,
        _collection_charge: collection_charge,
        _value_charges: value_charges,
    })
}

fn read_worker_byte_string_with_charge_from(
    input: &mut impl Read,
    maximum_length: usize,
    budget: &SessionAllocationBudget,
) -> io::Result<(Vec<u8>, SessionAllocationCharge)> {
    let length = usize::try_from(read_worker_integer_from(input)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "worker string exceeds limit"))?;
    if length > maximum_length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker string exceeds limit",
        ));
    }
    let padding_length = (8 - length % 8) % 8;
    let framed_length = length
        .checked_add(padding_length)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "worker string exceeds limit"))?;
    let charge = budget.charge(framed_length).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "worker metadata exceeds session limit",
        )
    })?;
    let mut framed = vec![0; framed_length];
    input.read_exact(&mut framed)?;
    if framed[length..].iter().any(|byte| *byte != 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker string padding is not zero",
        ));
    }
    framed.truncate(length);
    Ok((framed, charge))
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
        ProtocolSessionLimits, SERVER_WORKER_MAGIC, STDERR_LAST, SessionAllocationBudget,
        WorkerOperation, WorkerReader, WorkerVersion, protocol_name, read_client_worker_magic,
        read_fixture_client_handshake, read_fixture_client_post_handshake,
        read_fixture_server_handshake_info, read_fixture_set_options, read_fixture_terminal_frame,
        read_worker_byte_string, read_worker_integer, read_worker_operation,
        write_server_worker_magic, write_worker_byte_string, write_worker_integer,
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
    fn session_allocation_budget_releases_metadata_charges() {
        let limits = ProtocolSessionLimits::new(8, std::time::Duration::from_secs(1));
        let budget = SessionAllocationBudget::new(limits);

        let Ok(first) = budget.charge(8) else {
            panic!("first charge fits");
        };
        assert_eq!(budget.retained_bytes(), 8);
        assert!(matches!(budget.charge(1), Err(ProtocolError::SizeLimit)));
        drop(first);
        assert_eq!(budget.retained_bytes(), 0);
        let Ok(second) = budget.charge(8) else {
            panic!("released capacity is reusable");
        };
        drop(second);
    }

    #[test]
    fn session_limit_rejects_handshake_metadata_before_allocation() {
        let input = worker_handshake_with_feature(b"feature");
        let mut input = input.as_slice();
        let mut output = Vec::new();
        let limits = ProtocolSessionLimits::new(23, std::time::Duration::from_secs(1));
        let mut reader = WorkerReader::new(&mut input, limits);
        let error = reader
            .perform_server_handshake(&mut output, &[])
            .expect_err("metadata above the session limit is rejected");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "worker metadata exceeds session limit");
        assert_eq!(reader.retained_metadata_bytes(), 0);
    }

    #[test]
    fn negotiated_handshake_metadata_releases_its_session_charge() {
        let input = worker_handshake_with_feature(b"feature");
        let mut input = input.as_slice();
        let mut output = Vec::new();
        let limits = ProtocolSessionLimits::new(64, std::time::Duration::from_secs(1));
        let mut reader = WorkerReader::new(&mut input, limits);
        let Ok(negotiated) = reader.perform_server_handshake(&mut output, &["feature".to_owned()])
        else {
            panic!("handshake metadata fits");
        };

        assert!(reader.retained_metadata_bytes() > 0);
        drop(negotiated);
        assert_eq!(reader.retained_metadata_bytes(), 0);
    }

    fn worker_handshake_with_feature(feature: &[u8]) -> Vec<u8> {
        let mut input = Vec::new();
        write_worker_integer(&mut input, CLIENT_WORKER_MAGIC);
        write_worker_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
        write_worker_integer(&mut input, 1);
        write_worker_byte_string(&mut input, feature);
        input
    }

    #[test]
    fn worker_reader_uses_one_budget_for_live_metadata() {
        let input = worker_handshake_with_feature(b"feature");
        let mut input = input.as_slice();
        let mut output = Vec::new();
        let limits = ProtocolSessionLimits::new(64, std::time::Duration::from_secs(1));
        let mut reader = WorkerReader::new(&mut input, limits);

        let Ok(negotiated) = reader.perform_server_handshake(&mut output, &["feature".to_owned()])
        else {
            panic!("handshake metadata fits");
        };

        assert!(reader.retained_metadata_bytes() > 0);
        drop(negotiated);
        assert_eq!(reader.retained_metadata_bytes(), 0);
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
    fn parses_typed_fixture_handshake_messages_without_retaining_strings() {
        let mut client = Vec::new();
        write_worker_integer(&mut client, CLIENT_WORKER_MAGIC);
        write_worker_integer(&mut client, LATEST_WORKER_VERSION.to_wire());
        write_worker_integer(&mut client, 1);
        write_worker_byte_string(&mut client, b"secret-feature");
        let mut client = client.as_slice();

        let handshake = read_fixture_client_handshake(&mut client).unwrap();
        assert_eq!(handshake.version, LATEST_WORKER_VERSION);
        assert_eq!(handshake.feature_lengths, vec![14]);
        assert!(client.is_empty());

        let mut server = Vec::new();
        write_worker_integer(&mut server, SERVER_WORKER_MAGIC);
        write_worker_integer(&mut server, LATEST_WORKER_VERSION.to_wire());
        write_worker_integer(&mut server, 1);
        write_worker_byte_string(&mut server, b"secret-server-feature");
        let mut server = server.as_slice();
        let handshake = super::read_fixture_server_handshake(&mut server).unwrap();
        assert_eq!(handshake.version, LATEST_WORKER_VERSION);
        assert_eq!(handshake.feature_lengths, vec![21]);
        assert!(server.is_empty());

        let mut server_info = Vec::new();
        write_worker_byte_string(&mut server_info, b"secret-daemon-version");
        write_worker_integer(&mut server_info, 0);
        write_worker_integer(&mut server_info, STDERR_LAST);
        let mut server_info = server_info.as_slice();

        assert_eq!(
            read_fixture_server_handshake_info(&mut server_info, LATEST_WORKER_VERSION)
                .unwrap()
                .daemon_version_length,
            Some(21)
        );
        read_fixture_terminal_frame(&mut server_info).unwrap();
        assert!(server_info.is_empty());

        let mut post_handshake = Vec::new();
        write_worker_integer(&mut post_handshake, 0);
        write_worker_integer(&mut post_handshake, 0);
        let mut post_handshake = post_handshake.as_slice();
        read_fixture_client_post_handshake(&mut post_handshake, LATEST_WORKER_VERSION).unwrap();
        assert!(post_handshake.is_empty());
    }

    #[test]
    fn parses_typed_fixture_set_options_without_retaining_strings() {
        let mut input = Vec::new();
        write_worker_integer(&mut input, 19);
        for value in 0..12 {
            write_worker_integer(&mut input, value);
        }
        write_worker_integer(&mut input, 1);
        write_worker_byte_string(&mut input, b"secret-name");
        write_worker_byte_string(&mut input, b"secret-value");
        let mut input = input.as_slice();

        assert_eq!(
            read_fixture_set_options(&mut input),
            Ok(super::FixtureSetOptions {
                override_count: 1,
                string_lengths: vec![11, 12],
            })
        );
        assert!(input.is_empty());
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
    fn parses_a_fixture_bounded_store_path_request_without_retaining_its_body() {
        let mut input = Vec::new();
        write_worker_integer(&mut input, 11);
        write_worker_byte_string(&mut input, &[b'x'; 153]);
        let mut input = input.as_slice();

        assert_eq!(
            super::read_fixture_store_path_request(&mut input, WorkerOperation::AddTempRoot),
            Ok(super::FixtureStorePathRequest {
                operation: WorkerOperation::AddTempRoot,
                path_length: 153,
            })
        );
        assert!(input.is_empty());
    }

    #[test]
    fn rejects_oversized_truncated_or_malformed_fixture_store_path_requests() {
        let mut oversized = Vec::new();
        write_worker_integer(&mut oversized, 1);
        write_worker_integer(&mut oversized, 154);
        let mut oversized = oversized.as_slice();
        assert_eq!(
            super::read_fixture_store_path_request(&mut oversized, WorkerOperation::IsValidPath),
            Err(ProtocolError::SizeLimit)
        );

        let mut truncated = Vec::new();
        write_worker_integer(&mut truncated, 11);
        write_worker_integer(&mut truncated, 153);
        truncated.extend([0; 152]);
        let mut truncated = truncated.as_slice();
        assert_eq!(
            super::read_fixture_store_path_request(&mut truncated, WorkerOperation::AddTempRoot),
            Err(ProtocolError::Truncated)
        );

        let mut padded = Vec::new();
        write_worker_integer(&mut padded, 1);
        write_worker_byte_string(&mut padded, b"x");
        *padded.last_mut().expect("worker string padding") = 1;
        let mut padded = padded.as_slice();
        assert_eq!(
            super::read_fixture_store_path_request(&mut padded, WorkerOperation::IsValidPath),
            Err(ProtocolError::InternalFailure)
        );
    }

    #[test]
    fn parses_a_fixture_bounded_add_to_store_request_and_upload_without_retaining_bodies() {
        let mut input = Vec::new();
        write_worker_integer(&mut input, 7);
        write_worker_byte_string(&mut input, &[b'n'; 27]);
        write_worker_byte_string(&mut input, &[b'c'; 11]);
        write_worker_integer(&mut input, 0);
        write_worker_integer(&mut input, 0);
        write_worker_integer(&mut input, 502);
        input.extend(vec![b'x'; 502]);
        write_worker_integer(&mut input, 0);
        let mut input = input.as_slice();

        assert_eq!(
            super::read_fixture_add_to_store_request(&mut input),
            Ok(super::FixtureAddToStoreRequest {
                name_length: 27,
                content_address_length: 11,
                reference_count: 0,
                upload_chunk_lengths: vec![502],
                upload_length: 502,
            })
        );
        assert!(input.is_empty());
    }

    #[test]
    fn parses_fixture_bounded_query_missing_and_build_requests_without_retaining_paths() {
        let mut query_missing = Vec::new();
        write_worker_integer(&mut query_missing, 40);
        write_worker_integer(&mut query_missing, 1);
        write_worker_byte_string(&mut query_missing, &[b'd'; 157]);
        let mut query_missing = query_missing.as_slice();
        assert_eq!(
            super::read_fixture_derived_paths_request(
                &mut query_missing,
                WorkerOperation::QueryMissing
            ),
            Ok(super::FixtureDerivedPathsRequest {
                operation: WorkerOperation::QueryMissing,
                path_count: 1,
                path_lengths: vec![157],
                build_mode: None,
            })
        );

        let mut build = Vec::new();
        write_worker_integer(&mut build, 46);
        write_worker_integer(&mut build, 1);
        write_worker_byte_string(&mut build, b"fixture-derived-path");
        write_worker_integer(&mut build, 0);
        let mut build = build.as_slice();
        assert_eq!(
            super::read_fixture_derived_paths_request(
                &mut build,
                WorkerOperation::BuildPathsWithResults
            ),
            Ok(super::FixtureDerivedPathsRequest {
                operation: WorkerOperation::BuildPathsWithResults,
                path_count: 1,
                path_lengths: vec![20],
                build_mode: Some(0),
            })
        );
    }

    #[test]
    fn rejects_fixture_derived_path_count_length_and_build_mode_outside_the_envelope() {
        let mut too_many = Vec::new();
        write_worker_integer(&mut too_many, 40);
        write_worker_integer(&mut too_many, 2);
        let mut too_many = too_many.as_slice();
        assert_eq!(
            super::read_fixture_derived_paths_request(&mut too_many, WorkerOperation::QueryMissing),
            Err(ProtocolError::SizeLimit)
        );

        let mut too_long = Vec::new();
        write_worker_integer(&mut too_long, 46);
        write_worker_integer(&mut too_long, 1);
        write_worker_integer(&mut too_long, 158);
        let mut too_long = too_long.as_slice();
        assert_eq!(
            super::read_fixture_derived_paths_request(
                &mut too_long,
                WorkerOperation::BuildPathsWithResults
            ),
            Err(ProtocolError::SizeLimit)
        );

        let mut bad_mode = Vec::new();
        write_worker_integer(&mut bad_mode, 46);
        write_worker_integer(&mut bad_mode, 1);
        write_worker_byte_string(&mut bad_mode, b"path");
        write_worker_integer(&mut bad_mode, 3);
        let mut bad_mode = bad_mode.as_slice();
        assert_eq!(
            super::read_fixture_derived_paths_request(
                &mut bad_mode,
                WorkerOperation::BuildPathsWithResults
            ),
            Err(ProtocolError::InternalFailure)
        );
    }

    #[test]
    fn parses_fixture_bounded_query_missing_response_without_retaining_paths() {
        let mut input = Vec::new();
        write_worker_integer(&mut input, 1);
        write_worker_byte_string(&mut input, &[b'p'; 153]);
        write_worker_integer(&mut input, 0);
        write_worker_integer(&mut input, 0);
        write_worker_integer(&mut input, 502);
        write_worker_integer(&mut input, 502);
        let mut input = input.as_slice();

        assert_eq!(
            super::read_fixture_query_missing_response(&mut input),
            Ok(super::FixtureQueryMissingResponse {
                will_build_count: 1,
                will_substitute_count: 0,
                unknown_count: 0,
                path_lengths: vec![153],
            })
        );
        assert!(input.is_empty());
    }

    #[test]
    fn rejects_fixture_query_missing_response_outside_the_envelope() {
        let mut too_many = &2_u64.to_le_bytes()[..];
        assert_eq!(
            super::read_fixture_query_missing_response(&mut too_many),
            Err(ProtocolError::SizeLimit)
        );

        let mut truncated = Vec::new();
        write_worker_integer(&mut truncated, 1);
        write_worker_integer(&mut truncated, 153);
        let mut truncated = truncated.as_slice();
        assert_eq!(
            super::read_fixture_query_missing_response(&mut truncated),
            Err(ProtocolError::Truncated)
        );
    }

    #[test]
    fn parses_fixture_bounded_valid_path_info_replies_without_retaining_bodies() {
        let mut valid = Vec::new();
        write_worker_byte_string(&mut valid, &[b'p'; 153]);
        append_fixture_unkeyed_path_info(&mut valid);
        let mut valid = valid.as_slice();
        assert_eq!(
            super::read_fixture_valid_path_info(&mut valid),
            Ok(super::FixtureValidPathInfo {
                path_length: 153,
                deriver_length: 0,
                nar_hash_length: 64,
                reference_count: 0,
                signature_count: 0,
                content_address_length: 64,
            })
        );
        assert!(valid.is_empty());

        let mut query_path_info = Vec::new();
        write_worker_integer(&mut query_path_info, 1);
        append_fixture_unkeyed_path_info(&mut query_path_info);
        let mut query_path_info = query_path_info.as_slice();
        assert_eq!(
            super::read_fixture_query_path_info_response(&mut query_path_info),
            Ok(Some(super::FixtureUnkeyedPathInfo {
                deriver_length: 0,
                nar_hash_length: 64,
                reference_count: 0,
                signature_count: 0,
                content_address_length: 64,
            }))
        );
        assert!(query_path_info.is_empty());
    }

    #[test]
    fn rejects_fixture_path_info_outside_the_envelope() {
        let mut oversized = Vec::new();
        write_worker_integer(&mut oversized, 154);
        let mut oversized = oversized.as_slice();
        assert_eq!(
            super::read_fixture_valid_path_info(&mut oversized),
            Err(ProtocolError::SizeLimit)
        );

        let mut invalid_flag = &2_u64.to_le_bytes()[..];
        assert_eq!(
            super::read_fixture_query_path_info_response(&mut invalid_flag),
            Err(ProtocolError::InternalFailure)
        );
    }

    #[test]
    fn parses_fixture_bounded_build_paths_with_results_response_without_retaining_bodies() {
        let mut input = Vec::new();
        write_worker_integer(&mut input, 1);
        write_worker_byte_string(&mut input, &[b'p'; 157]);
        write_worker_integer(&mut input, 0);
        write_worker_byte_string(&mut input, b"");
        write_worker_integer(&mut input, 1);
        write_worker_integer(&mut input, 0);
        write_worker_integer(&mut input, 0);
        write_worker_integer(&mut input, 0);
        write_worker_integer(&mut input, 0);
        write_worker_integer(&mut input, 0);
        write_worker_integer(&mut input, 1);
        write_worker_byte_string(&mut input, &[b'o'; 75]);
        write_worker_byte_string(&mut input, &[b'r'; 196]);
        let mut input = input.as_slice();

        assert_eq!(
            super::read_fixture_build_paths_with_results_response(&mut input),
            Ok(super::FixtureBuildPathsWithResultsResponse {
                result_count: 1,
                path_length: 157,
                status: 0,
                error_length: 0,
                output_count: 1,
                output_id_length: 75,
                output_realisation_length: 196,
            })
        );
        assert!(input.is_empty());
    }

    #[test]
    fn rejects_fixture_build_results_outside_the_envelope() {
        let mut too_many = &2_u64.to_le_bytes()[..];
        assert_eq!(
            super::read_fixture_build_paths_with_results_response(&mut too_many),
            Err(ProtocolError::SizeLimit)
        );

        let mut bad_status = Vec::new();
        write_worker_integer(&mut bad_status, 1);
        write_worker_byte_string(&mut bad_status, b"p");
        write_worker_integer(&mut bad_status, 15);
        let mut bad_status = bad_status.as_slice();
        assert_eq!(
            super::read_fixture_build_paths_with_results_response(&mut bad_status),
            Err(ProtocolError::InternalFailure)
        );
    }

    #[test]
    fn parses_fixture_bounded_stderr_activity_frames_without_retaining_bodies() {
        let mut start = Vec::new();
        write_worker_integer(&mut start, super::STDERR_START_ACTIVITY);
        write_worker_integer(&mut start, 1);
        write_worker_integer(&mut start, 2);
        write_worker_integer(&mut start, 3);
        write_worker_byte_string(&mut start, &[b'm'; 164]);
        write_worker_integer(&mut start, 2);
        write_worker_integer(&mut start, 0);
        write_worker_integer(&mut start, 42);
        write_worker_integer(&mut start, 1);
        write_worker_byte_string(&mut start, &[b'f'; 153]);
        write_worker_integer(&mut start, 0);
        let mut start = start.as_slice();
        assert_eq!(
            super::read_fixture_stderr_frame(&mut start),
            Ok(super::FixtureStderrFrame::StartActivity {
                message_length: 164,
                field_count: 2,
                field_string_lengths: vec![153],
            })
        );
        assert!(start.is_empty());

        let mut next = Vec::new();
        write_worker_integer(&mut next, super::STDERR_NEXT);
        write_worker_byte_string(&mut next, &[b'n'; 145]);
        let mut next = next.as_slice();
        assert_eq!(
            super::read_fixture_stderr_frame(&mut next),
            Ok(super::FixtureStderrFrame::Next {
                message_length: 145
            })
        );
    }

    #[test]
    fn rejects_fixture_stderr_activity_frames_outside_the_envelope() {
        let mut too_many_fields = Vec::new();
        write_worker_integer(&mut too_many_fields, super::STDERR_RESULT);
        write_worker_integer(&mut too_many_fields, 1);
        write_worker_integer(&mut too_many_fields, 1);
        write_worker_integer(&mut too_many_fields, 5);
        let mut too_many_fields = too_many_fields.as_slice();
        assert_eq!(
            super::read_fixture_stderr_frame(&mut too_many_fields),
            Err(ProtocolError::SizeLimit)
        );

        let mut unknown = &0_u64.to_le_bytes()[..];
        assert_eq!(
            super::read_fixture_stderr_frame(&mut unknown),
            Err(ProtocolError::UnsupportedOperation)
        );
    }

    #[test]
    fn rejects_fixture_add_to_store_overflow_truncation_and_extra_upload_chunks() {
        let mut oversized = Vec::new();
        write_worker_integer(&mut oversized, 7);
        write_worker_integer(&mut oversized, 28);
        let mut oversized = oversized.as_slice();
        assert_eq!(
            super::read_fixture_add_to_store_request(&mut oversized),
            Err(ProtocolError::SizeLimit)
        );

        let mut truncated = Vec::new();
        write_worker_integer(&mut truncated, 7);
        write_worker_byte_string(&mut truncated, b"n");
        write_worker_byte_string(&mut truncated, b"c");
        write_worker_integer(&mut truncated, 0);
        write_worker_integer(&mut truncated, 0);
        write_worker_integer(&mut truncated, 1);
        let mut truncated = truncated.as_slice();
        assert_eq!(
            super::read_fixture_add_to_store_request(&mut truncated),
            Err(ProtocolError::Truncated)
        );

        let mut extra_chunk = Vec::new();
        write_worker_integer(&mut extra_chunk, 7);
        write_worker_byte_string(&mut extra_chunk, b"n");
        write_worker_byte_string(&mut extra_chunk, b"c");
        write_worker_integer(&mut extra_chunk, 0);
        write_worker_integer(&mut extra_chunk, 0);
        write_worker_integer(&mut extra_chunk, 1);
        extra_chunk.push(b'x');
        write_worker_integer(&mut extra_chunk, 1);
        extra_chunk.push(b'y');
        let mut extra_chunk = extra_chunk.as_slice();
        assert_eq!(
            super::read_fixture_add_to_store_request(&mut extra_chunk),
            Err(ProtocolError::SizeLimit)
        );

        let mut wrong_operation = &39_u64.to_le_bytes()[..];
        assert_eq!(
            super::read_fixture_add_to_store_request(&mut wrong_operation),
            Err(ProtocolError::UnsupportedOperation)
        );
    }

    fn append_fixture_unkeyed_path_info(output: &mut Vec<u8>) {
        write_worker_byte_string(output, b"");
        write_worker_byte_string(output, &[b'h'; 64]);
        write_worker_integer(output, 0);
        write_worker_integer(output, 0);
        write_worker_integer(output, 502);
        write_worker_integer(output, 1);
        write_worker_integer(output, 0);
        write_worker_byte_string(output, &[b'c'; 64]);
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
