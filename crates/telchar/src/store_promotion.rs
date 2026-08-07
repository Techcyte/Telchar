use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::nar::stage_nar;

pub const MAXIMUM_PROMOTION_REFERENCES: usize = 256;

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
    source: impl Read,
    staging_directory: &Path,
    store_directory: &Path,
    declared: &DeclaredPathInfo,
    backend: &mut impl StorePromotionBackend,
) -> io::Result<RegisteredPathInfo> {
    validate_declaration(declared, store_directory)?;
    let nar_path = create_staging_file(staging_directory)?;
    let result = promote_staged(
        source,
        nar_path.as_path(),
        staging_directory,
        store_directory,
        declared,
        backend,
    );
    let cleanup = std::fs::remove_file(&nar_path);
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(io::Error::other(format!(
            "staged NAR cleanup failed: {error}"
        ))),
    }
}

fn promote_staged(
    source: impl Read,
    nar_path: &Path,
    staging_directory: &Path,
    _store_directory: &Path,
    declared: &DeclaredPathInfo,
    backend: &mut impl StorePromotionBackend,
) -> io::Result<RegisteredPathInfo> {
    let mut staged = std::fs::File::create(nar_path)?;
    let fingerprint = stage_nar(source, &mut staged)?;
    if fingerprint.sha256 != declared.nar_hash {
        let error = io::Error::new(io::ErrorKind::InvalidData, "NAR hash mismatch");
        let _ = backend.is_valid_path(&declared.path);
        return Err(error);
    }
    if fingerprint.size != declared.nar_size {
        let error = io::Error::new(io::ErrorKind::InvalidData, "NAR size mismatch");
        let _ = backend.is_valid_path(&declared.path);
        return Err(error);
    }
    for reference in &declared.references {
        if reference != &declared.path && !backend.is_valid_path(reference)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "reference is not valid",
            ));
        }
    }
    staged.seek(SeekFrom::Start(0))?;
    let request = PromotionRequest {
        version: 1,
        store_uri: backend.store_uri().to_owned(),
        staging_directory: staging_directory.to_path_buf(),
        path: declared.path.clone(),
        nar_hash_hex: hex_hash(declared.nar_hash),
        nar_size: declared.nar_size,
        references: declared.references.clone(),
        deriver: declared.deriver.clone(),
        nar_path: nar_path.to_path_buf(),
    };
    backend.before_promote(&request)?;
    if let Err(error) = backend.promote(&request) {
        return match backend.is_valid_path(&declared.path) {
            Ok(false) => Err(io::Error::other(format!("promotion failed: {error}"))),
            Ok(true) => Err(io::Error::other(format!(
                "promotion failed but authoritative path is valid: {error}"
            ))),
            Err(query_error) => Err(io::Error::other(format!(
                "promotion failed and authoritative validity query failed: {error}; {query_error}"
            ))),
        };
    }
    let registered = backend.query_path_info(&declared.path)?;
    if registered.path != declared.path
        || registered.nar_hash != declared.nar_hash
        || registered.nar_size != declared.nar_size
        || registered.references != declared.references
        || registered.deriver != declared.deriver
        || registered.content_address != declared.content_address
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "registered metadata mismatch",
        ));
    }
    Ok(registered)
}

fn validate_declaration(declared: &DeclaredPathInfo, store_directory: &Path) -> io::Result<()> {
    if declared.content_address.is_some() || !declared.signatures.is_empty() || declared.ultimate {
        return Err(invalid("unsupported classic path metadata"));
    }
    if declared.references.len() > MAXIMUM_PROMOTION_REFERENCES {
        return Err(invalid("too many references"));
    }
    validate_store_path(&declared.path, store_directory, false)?;
    let mut references = std::collections::BTreeSet::new();
    for reference in &declared.references {
        validate_store_path(reference, store_directory, false)?;
        if !references.insert(reference) {
            return Err(invalid("invalid duplicate reference"));
        }
    }
    if let Some(deriver) = &declared.deriver {
        validate_store_path(deriver, store_directory, true)?;
    }
    Ok(())
}

fn validate_store_path(path: &Path, store_directory: &Path, deriver: bool) -> io::Result<()> {
    const HASH_LENGTH: usize = 32;
    const MAXIMUM_BASE_NAME_LENGTH: usize = 211;
    const HASH_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(invalid("invalid store path"));
    };
    if path.parent() != Some(store_directory)
        || file_name.len() > MAXIMUM_BASE_NAME_LENGTH
        || file_name.as_bytes().get(HASH_LENGTH) != Some(&b'-')
    {
        return Err(invalid("invalid store path"));
    }
    let Some(hash) = file_name.get(..HASH_LENGTH) else {
        return Err(invalid("invalid store path"));
    };
    let Some(name) = file_name.get(HASH_LENGTH + 1..) else {
        return Err(invalid("invalid store path"));
    };
    if !hash.bytes().all(|byte| HASH_ALPHABET.contains(&byte))
        || !valid_store_path_name(name)
        || name.ends_with(".drv") != deriver
    {
        return Err(invalid("invalid store path"));
    }
    Ok(())
}

fn valid_store_path_name(name: &str) -> bool {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.starts_with(".-")
        || name.starts_with("..-")
    {
        return false;
    }
    name.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
    })
}

fn create_staging_file(directory: &Path) -> io::Result<PathBuf> {
    for attempt in 0..100 {
        let path = directory.join(format!("telchar-nar-{}-{attempt}", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate staging file",
    ))
}

fn hex_hash(hash: [u8; 32]) -> String {
    hash.into_iter().map(|byte| format!("{byte:02x}")).collect()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
