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
    pub store_uri: String,
    pub staging_directory: PathBuf,
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

#[derive(serde::Deserialize)]
struct StorePathJson {
    #[serde(rename = "narHash")]
    nar_hash: String,
    #[serde(rename = "narSize")]
    nar_size: u64,
    references: Vec<PathBuf>,
    deriver: Option<PathBuf>,
    ca: Option<String>,
}

pub struct NixStorePromotionBackend {
    environment: Vec<(String, String)>,
    helper: PathBuf,
    store_uri: String,
}

impl NixStorePromotionBackend {
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

pub trait StorePromotionBackend {
    fn store_uri(&self) -> &str;
    fn is_valid_path(&mut self, path: &Path) -> io::Result<bool>;
    fn before_promote(&mut self, _request: &PromotionRequest) -> io::Result<()> {
        Ok(())
    }
    fn promote(&mut self, request: &PromotionRequest) -> io::Result<()>;
    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo>;
}

impl StorePromotionBackend for NixStorePromotionBackend {
    fn store_uri(&self) -> &str {
        &self.store_uri
    }

    fn is_valid_path(&mut self, path: &Path) -> io::Result<bool> {
        let output = std::process::Command::new("nix")
            .envs(self.environment.iter().cloned())
            .args(["--store", &self.store_uri, "path-info", "--json"])
            .arg(path)
            .output()?;
        if output.stdout.len() > 64 * 1024 || output.stderr.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "path-info response exceeds limit",
            ));
        }
        if output.status.success() {
            let entries: std::collections::BTreeMap<String, Option<serde_json::Value>> =
                serde_json::from_slice(&output.stdout).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid path-info JSON")
                })?;
            Ok(entries
                .get(path.to_string_lossy().as_ref())
                .is_some_and(Option::is_some))
        } else if String::from_utf8_lossy(&output.stderr).contains("is not valid") {
            Ok(false)
        } else {
            Err(io::Error::other("path-info query failed"))
        }
    }

    fn promote(&mut self, request: &PromotionRequest) -> io::Result<()> {
        let mut child = std::process::Command::new(&self.helper)
            .envs(self.environment.iter().cloned())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("promotion helper stdin not configured"))?;
        serde_json::to_writer(&mut stdin, request)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid helper request"))?;
        drop(stdin);
        let output = child.wait_with_output()?;
        if output.stdout.len() > 64 * 1024 || output.stderr.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "promotion helper output exceeds limit",
            ));
        }
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "promotion helper failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let result: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid helper response"))?;
        if result.get("version") != Some(&serde_json::Value::from(1))
            || result.get("promoted") != Some(&serde_json::Value::Bool(true))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected helper response",
            ));
        }
        Ok(())
    }

    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo> {
        let output = std::process::Command::new("nix")
            .envs(self.environment.iter().cloned())
            .args(["--store", &self.store_uri, "path-info", "--json"])
            .arg(path)
            .output()?;
        if !output.status.success() || output.stdout.len() > 64 * 1024 {
            return Err(io::Error::other("registered path-info query failed"));
        }
        let entries: std::collections::BTreeMap<String, StorePathJson> =
            serde_json::from_slice(&output.stdout).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid registered path-info JSON",
                )
            })?;
        let info = entries
            .get(path.to_string_lossy().as_ref())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "registered path omitted"))?;
        Ok(RegisteredPathInfo {
            path: path.to_path_buf(),
            nar_hash: parse_sha256_sri(&info.nar_hash)?,
            nar_size: info.nar_size,
            references: info.references.clone(),
            deriver: info.deriver.clone(),
            content_address: info.ca.clone(),
        })
    }
}

fn parse_sha256_sri(value: &str) -> io::Result<[u8; 32]> {
    use base64::Engine;

    let encoded = value
        .strip_prefix("sha256-")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unsupported NAR hash"))?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid NAR hash"))?;
    decoded
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SHA-256 length"))
}

pub fn validate_and_promote_nar(
    _source: impl Read,
    _staging_directory: &Path,
    _store_directory: &Path,
    _declared: &DeclaredPathInfo,
    _backend: &mut impl StorePromotionBackend,
) -> io::Result<RegisteredPathInfo> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "validated NAR promotion is not implemented",
    ))
}
