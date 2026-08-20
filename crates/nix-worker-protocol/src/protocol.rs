//! Defines worker protocol versions, operations, limits, and session allocation accounting.

use std::io::{self, Read};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use crate::{read_worker_integer, read_worker_integer_from, SessionAllocationCharges};

pub const CLIENT_WORKER_MAGIC: u64 = 0x6e69_7863;
pub const SERVER_WORKER_MAGIC: u64 = 0x6478_696f;
pub const MINIMUM_WORKER_VERSION: WorkerVersion = WorkerVersion::new(1, 35);
pub const LATEST_WORKER_VERSION: WorkerVersion = WorkerVersion::new(1, 38);
pub const FEATURE_NEGOTIATION_VERSION: WorkerVersion = WorkerVersion::new(1, 38);
pub const STDERR_NEXT: u64 = 0x6f6c_6d67;
pub const STDERR_LAST: u64 = 0x616c_7473;
pub const STDERR_ERROR: u64 = 0x6378_7470;
pub const STDERR_START_ACTIVITY: u64 = 0x5354_5254;
pub const STDERR_STOP_ACTIVITY: u64 = 0x5354_4f50;
pub const STDERR_RESULT: u64 = 0x5253_4c54;
pub const MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES: usize = 16 * 1024;
pub const MAXIMUM_STRUCTURED_FRAME_FIELDS: usize = 64;
pub const MAXIMUM_STRUCTURED_FRAME_FIELD_BYTES: usize = 4 * 1024;
pub(crate) const MAXIMUM_HANDSHAKE_FEATURES: usize = 64;
pub(crate) const MAXIMUM_HANDSHAKE_FEATURE_LENGTH: usize = 1024;
pub(crate) const NIX_STORE_DIRECTORY: &[u8] = b"/nix/store/";
pub(crate) const NIX_STORE_HASH_LENGTH: usize = 32;
pub(crate) const NIX_STORE_HASH_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

pub const MAXIMUM_QUERY_VALID_PATHS: usize = 65_536;
pub const MAXIMUM_ADD_MULTIPLE_TO_STORE_OBJECTS: usize = 256;
pub const MAXIMUM_ADD_MULTIPLE_TO_STORE_REFERENCES: usize = 256;
pub const MAXIMUM_ADD_MULTIPLE_TO_STORE_SIGNATURES: usize = 256;
pub const MAXIMUM_ADD_MULTIPLE_TO_STORE_HASH_BYTES: usize = 64;
pub const MAXIMUM_ADD_MULTIPLE_TO_STORE_SIGNATURE_BYTES: usize = 4096;
pub const MAXIMUM_ADD_MULTIPLE_TO_STORE_CONTENT_ADDRESS_BYTES: usize = 4096;
pub const MAXIMUM_WORKER_STORE_PATH_BYTES: usize = NIX_STORE_DIRECTORY.len() + 211;
pub const MAXIMUM_BUILD_DERIVATION_OUTPUTS: usize = 16;
pub const MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES: usize = 4096;
pub const MAXIMUM_BUILD_DERIVATION_ARGUMENTS: usize = 1024;
pub const MAXIMUM_BUILD_DERIVATION_ENVIRONMENT: usize = 4096;
pub const MAXIMUM_BUILD_DERIVATION_OUTPUT_NAME_BYTES: usize = 256;
pub const MAXIMUM_BUILD_DERIVATION_PLATFORM_BYTES: usize = 64;
pub const MAXIMUM_BUILD_DERIVATION_BUILDER_BYTES: usize = 4096;
pub const MAXIMUM_BUILD_DERIVATION_ARGUMENT_BYTES: usize = 64 * 1024;
pub const MAXIMUM_BUILD_DERIVATION_ENVIRONMENT_KEY_BYTES: usize = 4096;
pub const MAXIMUM_BUILD_DERIVATION_ENVIRONMENT_VALUE_BYTES: usize = 1024 * 1024;
pub const MAXIMUM_BUILD_DERIVATION_HASH_ALGORITHM_BYTES: usize = 64;
pub const MAXIMUM_BUILD_DERIVATION_HASH_BYTES: usize = 256;

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
    pub(crate) major: u8,
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
    pub(crate) _feature_charge: Option<SessionAllocationCharges>,
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
    pub(crate) const fn code(self) -> u64 {
        match self {
            Self::IsValidPath => 1,
            Self::QueryReferrers => 6,
            Self::AddToStore => 7,
            Self::AddTextToStore => 8,
            Self::BuildPaths => 9,
            Self::EnsurePath => 10,
            Self::AddTempRoot => 11,
            Self::AddIndirectRoot => 12,
            Self::SyncWithGc => 13,
            Self::FindRoots => 14,
            Self::QueryDeriver => 18,
            Self::SetOptions => 19,
            Self::CollectGarbage => 20,
            Self::QuerySubstitutablePathInfo => 21,
            Self::QueryDerivationOutputs => 22,
            Self::QueryAllValidPaths => 23,
            Self::QueryPathInfo => 26,
            Self::QueryDerivationOutputNames => 28,
            Self::QueryPathFromHashPart => 29,
            Self::QuerySubstitutablePathInfos => 30,
            Self::QueryValidPaths => 31,
            Self::QuerySubstitutablePaths => 32,
            Self::QueryValidDerivers => 33,
            Self::OptimiseStore => 34,
            Self::VerifyStore => 35,
            Self::BuildDerivation => 36,
            Self::AddSignatures => 37,
            Self::NarFromPath => 38,
            Self::AddToStoreNar => 39,
            Self::QueryMissing => 40,
            Self::QueryDerivationOutputMap => 41,
            Self::RegisterDrvOutput => 42,
            Self::QueryRealisation => 43,
            Self::AddMultipleToStore => 44,
            Self::AddBuildLog => 45,
            Self::BuildPathsWithResults => 46,
            Self::AddPermRoot => 47,
        }
    }

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
