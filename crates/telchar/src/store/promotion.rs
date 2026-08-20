//! Validates staged NAR metadata, imports it, and confirms authoritative gateway-store registration.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use nix_worker_protocol::AddToStoreNarInfo;

use crate::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

use crate::store::nar::stage_nar;

pub const MAXIMUM_PROMOTION_REFERENCES: usize = 256;
const MAXIMUM_SUBPROCESS_OUTPUT_BYTES: usize = 64 * 1024;

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
    pub content_address: Option<String>,
    pub signatures: Vec<String>,
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

pub struct GatewayStorePromotionBackend {
    endpoint: GatewayStoreEndpoint,
}

impl GatewayStorePromotionBackend {
    pub fn new(endpoint: GatewayStoreEndpoint) -> Self {
        Self { endpoint }
    }
}

impl StorePromotionBackend for GatewayStorePromotionBackend {
    fn store_uri(&self) -> &str {
        "configured-gateway-daemon"
    }

    fn is_valid_path(&mut self, path: &Path) -> io::Result<bool> {
        let mut connection = GatewayStoreConnection::connect(&self.endpoint)?;
        connection.is_valid_path(path.as_os_str().as_encoded_bytes())
    }

    fn promote(&mut self, request: &PromotionRequest) -> io::Result<()> {
        let mut nar = std::fs::File::open(&request.nar_path)?;
        let references = request
            .references
            .iter()
            .map(|path| path.as_os_str().as_encoded_bytes().to_vec())
            .collect::<Vec<_>>();
        let signatures = request
            .signatures
            .iter()
            .map(|signature| signature.as_bytes().to_vec())
            .collect::<Vec<_>>();
        let info = AddToStoreNarInfo {
            path: request.path.as_os_str().as_encoded_bytes(),
            deriver: request
                .deriver
                .as_ref()
                .map(|path| path.as_os_str().as_encoded_bytes()),
            nar_hash_hex: &request.nar_hash_hex,
            references: &references,
            registration_time: 0,
            nar_size: request.nar_size,
            ultimate: false,
            signatures: &signatures,
            content_address: request.content_address.as_deref().map(str::as_bytes),
        };
        let mut connection = GatewayStoreConnection::connect(&self.endpoint)?;
        connection.add_to_store_nar(&info, &mut nar, false, true)
    }

    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo> {
        let mut connection = GatewayStoreConnection::connect(&self.endpoint)?;
        let info = connection
            .query_path_info(path.as_os_str().as_encoded_bytes())?
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "registered path omitted"))?;
        Ok(RegisteredPathInfo {
            path: path.to_path_buf(),
            nar_hash: parse_sha256_hex(info.nar_hash_hex())?,
            nar_size: info.nar_size(),
            references: info
                .references()
                .iter()
                .map(|reference| PathBuf::from(String::from_utf8_lossy(reference).into_owned()))
                .collect(),
            deriver: info
                .deriver()
                .map(|deriver| PathBuf::from(String::from_utf8_lossy(deriver).into_owned())),
            content_address: info
                .content_address()
                .map(|address| String::from_utf8_lossy(address).into_owned()),
        })
    }
}

pub struct NixStorePromotionBackend {
    environment: Vec<(String, String)>,
    helper: PathBuf,
    nix: PathBuf,
    store_uri: String,
}

impl NixStorePromotionBackend {
    pub fn new(
        helper: impl Into<PathBuf>,
        store_uri: impl Into<String>,
        environment: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        let environment: Vec<_> = environment.into_iter().collect();
        let nix = environment
            .iter()
            .find_map(|(name, value)| (name == "TELCHAR_NIX").then(|| PathBuf::from(value)))
            .unwrap_or_else(|| PathBuf::from("nix"));
        Self {
            environment,
            helper: helper.into(),
            nix,
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
        let mut command = std::process::Command::new(&self.nix);
        command
            .envs(self.environment.iter().cloned())
            .args([
                "--extra-experimental-features",
                "nix-command",
                "--store",
                &self.store_uri,
                "path-info",
                "--json",
            ])
            .arg(path);
        let output = run_bounded_command(command, None)?;
        if output.exceeded_limit {
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
        let request = serde_json::to_vec(request)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid helper request"))?;
        let mut command = std::process::Command::new(&self.helper);
        command.envs(self.environment.iter().cloned());
        let output = run_bounded_command(command, Some(&request))?;
        if output.exceeded_limit {
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
        let mut command = std::process::Command::new(&self.nix);
        command
            .envs(self.environment.iter().cloned())
            .args([
                "--extra-experimental-features",
                "nix-command",
                "--store",
                &self.store_uri,
                "path-info",
                "--json",
            ])
            .arg(path);
        let output = run_bounded_command(command, None)?;
        if !output.status.success() || output.exceeded_limit {
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

struct BoundedCommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exceeded_limit: bool,
}

fn run_bounded_command(
    mut command: std::process::Command,
    input: Option<&[u8]>,
) -> io::Result<BoundedCommandOutput> {
    command
        .stdin(if input.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("subprocess stdout not configured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("subprocess stderr not configured"))?;
    let stdout_reader = std::thread::spawn(move || drain_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || drain_bounded(stderr));
    if let Some(input) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("subprocess stdin not configured"))?;
        stdin.write_all(input)?;
    }
    let status = child.wait()?;
    let (stdout, stdout_exceeded) = stdout_reader
        .join()
        .map_err(|_| io::Error::other("subprocess stdout reader panicked"))??;
    let (stderr, stderr_exceeded) = stderr_reader
        .join()
        .map_err(|_| io::Error::other("subprocess stderr reader panicked"))??;
    Ok(BoundedCommandOutput {
        status,
        stdout,
        stderr,
        exceeded_limit: stdout_exceeded || stderr_exceeded,
    })
}

fn drain_bounded(mut source: impl Read) -> io::Result<(Vec<u8>, bool)> {
    let mut retained = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            return Ok((retained, exceeded));
        }
        let available = MAXIMUM_SUBPROCESS_OUTPUT_BYTES.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(available)]);
        exceeded |= read > available;
    }
}

fn parse_sha256_hex(value: &str) -> io::Result<[u8; 32]> {
    if value.len() != 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid NAR hash",
        ));
    }
    let mut hash = [0_u8; 32];
    for (output, pair) in hash.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *output = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Ok(hash)
}

fn hex_value(value: u8) -> io::Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid NAR hash",
        )),
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
        content_address: declared.content_address.clone(),
        signatures: declared.signatures.clone(),
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
    if declared.ultimate {
        return Err(invalid("unsupported classic path metadata"));
    }
    if declared
        .content_address
        .as_deref()
        .is_some_and(|address| !valid_fixed_content_address(address))
    {
        return Err(invalid("invalid fixed-output content address"));
    }
    if declared.references.len() > MAXIMUM_PROMOTION_REFERENCES {
        return Err(invalid("too many references"));
    }
    validate_store_path(&declared.path, store_directory, None)?;
    let mut references = std::collections::BTreeSet::new();
    for reference in &declared.references {
        validate_store_path(reference, store_directory, None)?;
        if !references.insert(reference) {
            return Err(invalid("invalid duplicate reference"));
        }
    }
    if let Some(deriver) = &declared.deriver {
        validate_store_path(deriver, store_directory, Some(true))?;
    }
    Ok(())
}

fn valid_fixed_content_address(address: &str) -> bool {
    let Some(rest) = address.strip_prefix("fixed:") else {
        return false;
    };
    let rest = rest.strip_prefix("r:").unwrap_or(rest);
    let Some((algorithm, hash)) = rest.split_once(':') else {
        return false;
    };
    let expected_length = match algorithm {
        "md5" => 26,
        "sha1" => 32,
        "sha256" => 52,
        "sha512" => 103,
        _ => return false,
    };
    hash.len() == expected_length
        && hash
            .bytes()
            .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&byte))
}

fn validate_store_path(
    path: &Path,
    store_directory: &Path,
    deriver: Option<bool>,
) -> io::Result<()> {
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
        || deriver.is_some_and(|deriver| name.ends_with(".drv") != deriver)
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
