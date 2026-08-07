use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::store_promotion::RegisteredPathInfo;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreExportRequest {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedStoreExport {
    pub metadata: RegisteredPathInfo,
    pub nar_hash: [u8; 32],
    pub nar_size: u64,
}

pub trait StoreExportBackend {
    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo>;
    fn export_nar(&mut self, request: &StoreExportRequest, sink: &mut dyn Write) -> io::Result<()>;
}

pub fn export_verified_nar(
    _path: &Path,
    _sink: &mut impl Write,
    _backend: &mut impl StoreExportBackend,
) -> io::Result<VerifiedStoreExport> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "verified store export is not implemented",
    ))
}
