use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::store_promotion::RegisteredPathInfo;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StoreExportRequest {
    pub version: u32,
    pub store_uri: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedStoreExport {
    pub metadata: RegisteredPathInfo,
    pub nar_hash: [u8; 32],
    pub nar_size: u64,
}

pub trait StoreExportBackend {
    fn store_uri(&self) -> &str;
    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo>;
    fn export_nar(&mut self, request: &StoreExportRequest, sink: &mut dyn Write) -> io::Result<()>;
}

pub struct NixStoreExportBackend {
    environment: Vec<(String, String)>,
    helper: PathBuf,
    store_uri: String,
}

impl NixStoreExportBackend {
    pub fn new(
        helper: impl Into<PathBuf>,
        store_uri: impl Into<String>,
        environment: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        Self {
            environment: environment.into_iter().collect(),
            helper: helper.into(),
            store_uri: store_uri.into(),
        }
    }
}

impl StoreExportBackend for NixStoreExportBackend {
    fn store_uri(&self) -> &str {
        &self.store_uri
    }

    fn query_path_info(&mut self, _path: &Path) -> io::Result<RegisteredPathInfo> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "verified store export metadata query is not implemented",
        ))
    }

    fn export_nar(
        &mut self,
        _request: &StoreExportRequest,
        _sink: &mut dyn Write,
    ) -> io::Result<()> {
        let _ = (&self.environment, &self.helper);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "verified store export helper is not implemented",
        ))
    }
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
