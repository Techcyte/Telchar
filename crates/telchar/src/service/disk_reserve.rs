//! Checks filesystem free-space reserves before admitting builds and NAR transfers.

use std::collections::BTreeMap;
use std::io;
use std::sync::Mutex;

static RESERVED_BYTES: Mutex<BTreeMap<u64, u64>> = Mutex::new(BTreeMap::new());

#[derive(Debug)]
pub struct DiskReservation {
    filesystems: Vec<(u64, u64)>,
}

impl Drop for DiskReservation {
    fn drop(&mut self) {
        let mut reserved = RESERVED_BYTES.lock().unwrap_or_else(|p| p.into_inner());
        for (identity, bytes) in &self.filesystems {
            if let Some(total) = reserved.get_mut(identity) {
                *total -= bytes;
                if *total == 0 {
                    reserved.remove(identity);
                }
            }
        }
    }
}
use std::path::Path;

pub const DEFAULT_GATEWAY_DISK_RESERVE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
pub const GATEWAY_STORE_DIRECTORY: &str = "/nix/store";

pub fn gateway_store_directory() -> io::Result<std::path::PathBuf> {
    let path = std::env::var_os("TELCHAR_GATEWAY_STORE_DIRECTORY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| GATEWAY_STORE_DIRECTORY.into());
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "gateway store directory must be absolute",
        ));
    }
    Ok(path)
}

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
        match measure(probe, store_directory, "gateway-store") {
            Some(store) => admit("gateway-store", store.available_bytes, self.bytes),
            None => Ok(()),
        }
    }

    pub fn admit_transfer(
        self,
        probe: &dyn DiskReserveProbe,
        store_directory: &Path,
        staging_directory: &Path,
        nar_size: u64,
    ) -> Result<(), AdmissionFailure> {
        self.admit_staging(probe, store_directory, staging_directory, nar_size, 1)
    }

    pub fn reserve_transfer(
        self,
        probe: &dyn DiskReserveProbe,
        store_directory: &Path,
        staging_directory: &Path,
        nar_size: u64,
    ) -> Result<DiskReservation, AdmissionFailure> {
        self.reserve_staging(probe, store_directory, staging_directory, nar_size, 1)
    }

    pub fn reserve_promotion(
        self,
        probe: &dyn DiskReserveProbe,
        store_directory: &Path,
        staging_directory: &Path,
        nar_size: u64,
    ) -> Result<DiskReservation, AdmissionFailure> {
        self.reserve_staging(probe, store_directory, staging_directory, nar_size, 2)
    }

    fn reserve_staging(
        self,
        probe: &dyn DiskReserveProbe,
        store_directory: &Path,
        staging_directory: &Path,
        nar_size: u64,
        staging_copies: u64,
    ) -> Result<DiskReservation, AdmissionFailure> {
        let mut reserved = RESERVED_BYTES.lock().unwrap_or_else(|p| p.into_inner());
        let store = measure(probe, store_directory, "gateway-store").ok_or_else(|| {
            AdmissionFailure::failed("gateway-store", RejectionReason::ProbeFailed)
        })?;
        let staging = measure(probe, staging_directory, "staging")
            .ok_or_else(|| AdmissionFailure::failed("staging", RejectionReason::ProbeFailed))?;
        let staging_bytes = nar_size.checked_mul(staging_copies).ok_or_else(|| {
            AdmissionFailure::failed("staging", RejectionReason::ArithmeticOverflow)
        })?;
        let mut requested = BTreeMap::new();
        requested.insert(store.identity, (nar_size, store.available_bytes));
        let entry = requested
            .entry(staging.identity)
            .or_insert((0, staging.available_bytes));
        entry.0 = entry.0.checked_add(staging_bytes).ok_or_else(|| {
            AdmissionFailure::failed("shared", RejectionReason::ArithmeticOverflow)
        })?;
        entry.1 = entry.1.min(staging.available_bytes);
        for (identity, (bytes, available)) in &requested {
            let required = reserved
                .get(identity)
                .copied()
                .unwrap_or(0)
                .checked_add(*bytes)
                .and_then(|total| total.checked_add(self.bytes))
                .ok_or_else(|| {
                    AdmissionFailure::failed("shared", RejectionReason::ArithmeticOverflow)
                })?;
            admit("shared", *available, required)?;
        }
        let filesystems = requested
            .into_iter()
            .map(|(identity, (bytes, _))| {
                *reserved.entry(identity).or_default() += bytes;
                (identity, bytes)
            })
            .collect();
        Ok(DiskReservation { filesystems })
    }

    pub fn admit_promotion(
        self,
        probe: &dyn DiskReserveProbe,
        store_directory: &Path,
        staging_directory: &Path,
        nar_size: u64,
    ) -> Result<(), AdmissionFailure> {
        self.admit_staging(probe, store_directory, staging_directory, nar_size, 2)
    }

    fn admit_staging(
        self,
        probe: &dyn DiskReserveProbe,
        store_directory: &Path,
        staging_directory: &Path,
        nar_size: u64,
        staging_copies: u64,
    ) -> Result<(), AdmissionFailure> {
        let staging_bytes = nar_size.checked_mul(staging_copies).ok_or_else(|| {
            AdmissionFailure::failed("staging", RejectionReason::ArithmeticOverflow)
        })?;
        let store = measure(probe, store_directory, "gateway-store").ok_or_else(|| {
            AdmissionFailure::failed("gateway-store", RejectionReason::ProbeFailed)
        })?;
        let staging = measure(probe, staging_directory, "staging")
            .ok_or_else(|| AdmissionFailure::failed("staging", RejectionReason::ProbeFailed))?;
        self.admit_transfer_filesystems(store, staging, nar_size, staging_bytes)
    }

    fn admit_copy(
        self,
        filesystem: &'static str,
        measured: Filesystem,
        nar_size: u64,
    ) -> Result<(), AdmissionFailure> {
        let required = self.bytes.checked_add(nar_size).ok_or_else(|| {
            AdmissionFailure::failed(filesystem, RejectionReason::ArithmeticOverflow)
        })?;
        admit(filesystem, measured.available_bytes, required)
    }

    fn admit_transfer_filesystems(
        self,
        store: Filesystem,
        staging: Filesystem,
        nar_size: u64,
        staging_bytes: u64,
    ) -> Result<(), AdmissionFailure> {
        if store.identity == staging.identity {
            let required = nar_size
                .checked_add(staging_bytes)
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
            self.admit_copy("staging", staging, staging_bytes)
        }
    }
}

fn measure(
    probe: &dyn DiskReserveProbe,
    path: &Path,
    filesystem: &'static str,
) -> Option<Filesystem> {
    match probe.probe(path) {
        Ok(measured) => Some(measured),
        Err(error) => {
            let reason = match error {
                ProbeError::Failed => "probe-failed",
                ProbeError::ArithmeticOverflow => "arithmetic-overflow",
            };
            tracing::warn!(
                event = "worker.disk_reserve.probe_failed",
                filesystem,
                reason,
                "Capacity measurement unavailable"
            );
            None
        }
    }
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
