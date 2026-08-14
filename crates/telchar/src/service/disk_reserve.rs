//! Checks filesystem free-space reserves before admitting builds and NAR transfers.

use std::io;
use std::path::Path;

pub const DEFAULT_GATEWAY_DISK_RESERVE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
pub const GATEWAY_STORE_DIRECTORY: &str = "/nix/store";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Filesystem {
    identity: u64,
    available_bytes: u64,
}

impl Filesystem {
    pub const fn new(identity: u64, available_bytes: u64) -> Self {
        Self {
            identity,
            available_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectionReason {
    InsufficientSpace,
    ProbeFailed,
    ArithmeticOverflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionFailure {
    filesystem: &'static str,
    required_bytes: u64,
    available_bytes: Option<u64>,
    reason: RejectionReason,
}

impl AdmissionFailure {
    const fn insufficient(
        filesystem: &'static str,
        required_bytes: u64,
        available_bytes: u64,
    ) -> Self {
        Self {
            filesystem,
            required_bytes,
            available_bytes: Some(available_bytes),
            reason: RejectionReason::InsufficientSpace,
        }
    }

    const fn failed(filesystem: &'static str, reason: RejectionReason) -> Self {
        Self {
            filesystem,
            required_bytes: 0,
            available_bytes: None,
            reason,
        }
    }

    pub const fn filesystem(self) -> &'static str {
        self.filesystem
    }

    pub const fn required_bytes(self) -> u64 {
        self.required_bytes
    }

    pub const fn available_bytes(self) -> Option<u64> {
        self.available_bytes
    }

    pub const fn reason(self) -> RejectionReason {
        self.reason
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeError {
    Failed,
    ArithmeticOverflow,
}

pub trait DiskReserveProbe: Send + Sync {
    fn probe(&self, path: &Path) -> Result<Filesystem, ProbeError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OsDiskReserveProbe;

impl DiskReserveProbe for OsDiskReserveProbe {
    fn probe(&self, path: &Path) -> Result<Filesystem, ProbeError> {
        let metadata = std::fs::metadata(path).map_err(|_| ProbeError::Failed)?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt as _;
            metadata.dev()
        };
        #[cfg(not(unix))]
        let identity = return Err(ProbeError::Failed);
        let statistics = rustix::fs::statvfs(path).map_err(|_| ProbeError::Failed)?;
        let available_bytes = statistics
            .f_bavail
            .checked_mul(statistics.f_frsize)
            .ok_or(ProbeError::ArithmeticOverflow)?;
        Ok(Filesystem::new(identity, available_bytes))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskReserve {
    bytes: u64,
}

impl Default for DiskReserve {
    fn default() -> Self {
        Self {
            bytes: DEFAULT_GATEWAY_DISK_RESERVE_BYTES,
        }
    }
}

impl DiskReserve {
    pub fn parse(value: &str) -> io::Result<Self> {
        let bytes = value
            .parse::<u64>()
            .ok()
            .filter(|bytes| *bytes > 0)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid gateway disk reserve")
            })?;
        Ok(Self { bytes })
    }

    pub fn from_environment() -> io::Result<Self> {
        let default = Self::default();
        Self::parse(
            &std::env::var("TELCHAR_GATEWAY_DISK_RESERVE_BYTES")
                .unwrap_or_else(|_| default.bytes.to_string()),
        )
    }

    pub const fn bytes(self) -> u64 {
        self.bytes
    }

    pub fn admit_build(
        self,
        probe: &dyn DiskReserveProbe,
        store_directory: &Path,
    ) -> Result<(), AdmissionFailure> {
        let store = probe
            .probe(store_directory)
            .map_err(|error| failure("gateway-store", error))?;
        admit("gateway-store", store.available_bytes, self.bytes)
    }

    pub fn admit_transfer(
        self,
        probe: &dyn DiskReserveProbe,
        store_directory: &Path,
        staging_directory: &Path,
        nar_size: u64,
    ) -> Result<(), AdmissionFailure> {
        let store = probe
            .probe(store_directory)
            .map_err(|error| failure("gateway-store", error))?;
        let staging = probe
            .probe(staging_directory)
            .map_err(|error| failure("staging", error))?;
        self.admit_transfer_filesystems(store, staging, nar_size)
    }

    fn admit_transfer_filesystems(
        self,
        store: Filesystem,
        staging: Filesystem,
        nar_size: u64,
    ) -> Result<(), AdmissionFailure> {
        if store.identity == staging.identity {
            let required = nar_size
                .checked_mul(2)
                .and_then(|bytes| self.bytes.checked_add(bytes))
                .ok_or_else(|| {
                    AdmissionFailure::failed("shared", RejectionReason::ArithmeticOverflow)
                })?;
            admit(
                "shared",
                store.available_bytes.min(staging.available_bytes),
                required,
            )
        } else {
            let required = self.bytes.checked_add(nar_size).ok_or_else(|| {
                AdmissionFailure::failed("gateway-store", RejectionReason::ArithmeticOverflow)
            })?;
            admit("gateway-store", store.available_bytes, required)?;
            admit("staging", staging.available_bytes, required)
        }
    }
}

fn failure(filesystem: &'static str, error: ProbeError) -> AdmissionFailure {
    AdmissionFailure::failed(
        filesystem,
        match error {
            ProbeError::Failed => RejectionReason::ProbeFailed,
            ProbeError::ArithmeticOverflow => RejectionReason::ArithmeticOverflow,
        },
    )
}

fn admit(
    filesystem: &'static str,
    available_bytes: u64,
    required_bytes: u64,
) -> Result<(), AdmissionFailure> {
    if available_bytes < required_bytes {
        Err(AdmissionFailure::insufficient(
            filesystem,
            required_bytes,
            available_bytes,
        ))
    } else {
        Ok(())
    }
}
