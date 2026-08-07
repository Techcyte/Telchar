use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclaredPathInfo {
    pub path: PathBuf,
    pub nar_hash: [u8; 32],
    pub nar_size: u64,
    pub references: Vec<PathBuf>,
    pub deriver: Option<PathBuf>,
    pub content_address: Option<String>,
    pub signatures: Vec<String>,
    pub ultimate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PromotionRequest {
    pub version: u32,
    pub path: PathBuf,
    pub nar_hash_hex: String,
    pub nar_size: u64,
    pub references: Vec<PathBuf>,
    pub deriver: Option<PathBuf>,
    pub nar_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredPathInfo {
    pub path: PathBuf,
    pub nar_hash: [u8; 32],
    pub nar_size: u64,
    pub references: Vec<PathBuf>,
    pub deriver: Option<PathBuf>,
    pub content_address: Option<String>,
}

pub trait StorePromotionBackend {
    fn is_valid_path(&mut self, path: &Path) -> io::Result<bool>;
    fn promote(&mut self, request: &PromotionRequest) -> io::Result<()>;
    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo>;
}

pub fn validate_and_promote_nar(
    _source: impl Read,
    _staging_directory: &Path,
    _declared: &DeclaredPathInfo,
    _backend: &mut impl StorePromotionBackend,
) -> io::Result<RegisteredPathInfo> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "validated NAR promotion is not implemented",
    ))
}
