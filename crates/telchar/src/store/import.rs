//! Imports declared NARs into the gateway store through the typed worker protocol.

use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use nix_worker_protocol::AddMultipleToStorePathInfo;

use crate::store::daemon::GatewayStoreEndpoint;
use crate::store::promotion::{
    validate_and_promote_nar, DeclaredPathInfo, GatewayStorePromotionBackend,
};

static IMPORT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

pub fn importer_from_environment() -> io::Result<Box<dyn StoreImportBackend>> {
    if std::env::var_os("TELCHAR_GATEWAY_STORE_URI").is_none() {
        return Ok(Box::new(UnavailableStoreImport));
    }
    Ok(Box::new(GatewayStoreImport::from_environment()?))
}

pub trait StoreImportBackend {
    fn staging_directory(&self) -> Option<&Path>;

    fn import(
        &mut self,
        info: &AddMultipleToStorePathInfo,
        source: &mut dyn Read,
    ) -> io::Result<()>;
}

struct UnavailableStoreImport;

impl StoreImportBackend for UnavailableStoreImport {
    fn staging_directory(&self) -> Option<&Path> {
        None
    }

    fn import(
        &mut self,
        _info: &AddMultipleToStorePathInfo,
        _source: &mut dyn Read,
    ) -> io::Result<()> {
        Err(unavailable("gateway store endpoint is not configured"))
    }
}

pub struct GatewayStoreImport {
    backend: GatewayStorePromotionBackend,
    staging_directory: PathBuf,
}

impl GatewayStoreImport {
    pub fn new(endpoint: GatewayStoreEndpoint) -> io::Result<Self> {
        let staging_root = std::env::temp_dir();
        std::fs::create_dir_all(&staging_root).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "create import staging root {} failed: {error}",
                    staging_root.display()
                ),
            )
        })?;
        let staging_directory = staging_root.join(format!(
            "telchar-import-{}-{}",
            std::process::id(),
            IMPORT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&staging_directory).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "create import staging directory {} failed: {error}",
                    staging_directory.display()
                ),
            )
        })?;
        std::fs::set_permissions(&staging_directory, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            backend: GatewayStorePromotionBackend::new(endpoint),
            staging_directory,
        })
    }

    pub fn from_environment() -> io::Result<Self> {
        let endpoint = std::env::var_os("TELCHAR_GATEWAY_STORE_URI")
            .ok_or_else(|| unavailable("gateway store endpoint is not configured"))
            .and_then(|value| {
                GatewayStoreEndpoint::parse_os(&value)
                    .map_err(|_| unavailable("gateway store endpoint is invalid"))
            })?;
        Self::new(endpoint)
    }
}

impl StoreImportBackend for GatewayStoreImport {
    fn staging_directory(&self) -> Option<&Path> {
        Some(&self.staging_directory)
    }

    fn import(
        &mut self,
        info: &AddMultipleToStorePathInfo,
        source: &mut dyn Read,
    ) -> io::Result<()> {
        let declared = declared_path_info(info)?;
        validate_and_promote_nar(
            source,
            &self.staging_directory,
            Path::new("/nix/store"),
            &declared,
            &mut self.backend,
        )
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("store input promotion failed: {error}"),
            )
        })?;
        Ok(())
    }
}

impl Drop for GatewayStoreImport {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.staging_directory);
    }
}

fn declared_path_info(info: &AddMultipleToStorePathInfo) -> io::Result<DeclaredPathInfo> {
    Ok(DeclaredPathInfo {
        path: path(info.path())?,
        nar_hash: parse_sha256_hex(info.nar_hash())?,
        nar_size: info.nar_size(),
        references: info
            .references()
            .iter()
            .map(|reference| path(reference))
            .collect::<io::Result<_>>()?,
        deriver: info.deriver().map(path).transpose()?,
        content_address: None,
        signatures: info
            .signatures()
            .iter()
            .map(|signature| text(signature))
            .collect::<io::Result<_>>()?,
        ultimate: info.ultimate(),
    })
}

fn path(value: &[u8]) -> io::Result<PathBuf> {
    Ok(PathBuf::from(text(value)?))
}

fn text(value: &[u8]) -> io::Result<String> {
    String::from_utf8(value.to_vec())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid store metadata"))
}

fn parse_sha256_hex(value: &[u8]) -> io::Result<[u8; 32]> {
    if value.len() != 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid NAR hash",
        ));
    }
    let mut hash = [0_u8; 32];
    for (output, pair) in hash.iter_mut().zip(value.chunks_exact(2)) {
        *output = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
    }
    Ok(hash)
}

fn hex_digit(value: u8) -> io::Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid NAR hash",
        )),
    }
}

fn unavailable(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, message)
}
