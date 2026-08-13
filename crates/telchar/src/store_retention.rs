//! Maintains durable GC roots and reconciles released or expired retained outputs.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionEntry {
    lease_id: String,
    store_path: String,
}

impl RetentionEntry {
    pub fn new(lease_id: impl Into<String>, store_path: impl Into<String>) -> Self {
        Self {
            lease_id: lease_id.into(),
            store_path: store_path.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedPath {
    lease_id: String,
    store_path: String,
    root_path: PathBuf,
    created: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleasedRetentionEntry {
    lease_id: String,
    store_path: String,
}

impl ReleasedRetentionEntry {
    pub fn new(lease_id: impl Into<String>, store_path: impl Into<String>) -> Self {
        Self {
            lease_id: lease_id.into(),
            store_path: store_path.into(),
        }
    }
}

pub trait StoreRetentionBackend: Send {
    fn retain(&mut self, entries: &[RetentionEntry]) -> io::Result<Vec<RetainedPath>>;
    fn rollback(&mut self, retained: &[RetainedPath]) -> io::Result<()>;
    fn release(&mut self, released: &[ReleasedRetentionEntry]) -> io::Result<()>;
}

pub fn reconcile_released_request_leases(
    database_url: &str,
    backend: &mut dyn StoreRetentionBackend,
) -> io::Result<()> {
    let mut after_lease_id = None;
    loop {
        let leases = crate::persistence::read_released_request_leases_page(
            database_url,
            after_lease_id.as_deref(),
            256,
        )
        .map_err(|_| retention_error())?;
        let page_len = leases.len();
        let entries = leases
            .iter()
            .map(|lease| ReleasedRetentionEntry::new(&lease.lease_id, &lease.store_path))
            .collect::<Vec<_>>();
        backend.release(&entries)?;
        if page_len < 256 {
            return Ok(());
        }
        after_lease_id = leases.last().map(|lease| lease.lease_id.clone());
    }
}

pub fn reconcile_output_retention(
    database_url: &str,
    backend: &mut dyn StoreRetentionBackend,
    now: SystemTime,
) -> io::Result<()> {
    let result = (|| {
        reconcile_released_request_leases(database_url, backend)?;
        let mut after_lease_id = None;
        loop {
            let released = crate::persistence::release_expired_request_output_leases(
                database_url,
                now,
                after_lease_id.as_deref(),
                256,
            )
            .map_err(|_| retention_error())?;
            if released.is_empty() {
                return Ok(());
            }
            let entries = released
                .iter()
                .map(|lease| ReleasedRetentionEntry::new(&lease.lease_id, &lease.store_path))
                .collect::<Vec<_>>();
            backend.release(&entries)?;
            after_lease_id = released.last().map(|lease| lease.lease_id.clone());
            if released.len() < 256 {
                return Ok(());
            }
        }
    })();
    match &result {
        Ok(()) => tracing::info!(
            event = "gateway.output_retention.reconciled",
            operation = "expire-output-retention",
            result = "succeeded",
        ),
        Err(_) => tracing::warn!(
            event = "gateway.output_retention.reconciled",
            operation = "expire-output-retention",
            result = "failed",
        ),
    }
    result
}

pub fn backend_from_environment() -> io::Result<Box<dyn StoreRetentionBackend>> {
    let store_uri = std::env::var("TELCHAR_GATEWAY_STORE_URI").ok();
    let root_directory = std::env::var_os("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY");
    match (store_uri, root_directory) {
        (Some(_), Some(root_directory))
            if std::env::var_os("TELCHAR_TEST_STORE_RETENTION").is_some() =>
        {
            Ok(Box::new(FilesystemStoreRetentionBackend::new(
                root_directory,
            )?))
        }
        (Some(store_uri), Some(root_directory)) => Ok(Box::new(NixStoreRetentionBackend::new(
            store_uri,
            root_directory,
        )?)),
        _ => Ok(Box::new(UnavailableStoreRetentionBackend)),
    }
}

struct FilesystemStoreRetentionBackend {
    root_directory: PathBuf,
}

impl FilesystemStoreRetentionBackend {
    fn new(root_directory: impl Into<PathBuf>) -> io::Result<Self> {
        Ok(Self {
            root_directory: validate_root_directory(root_directory.into())?,
        })
    }
}

impl StoreRetentionBackend for FilesystemStoreRetentionBackend {
    fn retain(&mut self, entries: &[RetentionEntry]) -> io::Result<Vec<RetainedPath>> {
        validate_entries(entries, &self.root_directory)?;
        let mut retained = Vec::with_capacity(entries.len());
        for entry in entries {
            let root_path = self.root_directory.join(&entry.lease_id);
            let created = create_root(&root_path, &entry.store_path)?;
            retained.push(RetainedPath {
                lease_id: entry.lease_id.clone(),
                store_path: entry.store_path.clone(),
                root_path,
                created,
            });
        }
        Ok(retained)
    }

    fn rollback(&mut self, retained: &[RetainedPath]) -> io::Result<()> {
        rollback_paths(&self.root_directory, retained)
    }

    fn release(&mut self, released: &[ReleasedRetentionEntry]) -> io::Result<()> {
        release_paths(&self.root_directory, released)
    }
}

struct UnavailableStoreRetentionBackend;

impl StoreRetentionBackend for UnavailableStoreRetentionBackend {
    fn retain(&mut self, entries: &[RetentionEntry]) -> io::Result<Vec<RetainedPath>> {
        if entries.is_empty() {
            Ok(Vec::new())
        } else {
            Err(retention_error())
        }
    }

    fn rollback(&mut self, retained: &[RetainedPath]) -> io::Result<()> {
        if retained.is_empty() {
            Ok(())
        } else {
            Err(retention_error())
        }
    }

    fn release(&mut self, released: &[ReleasedRetentionEntry]) -> io::Result<()> {
        if released.is_empty() {
            Ok(())
        } else {
            Err(retention_error())
        }
    }
}

pub struct NixStoreRetentionBackend {
    socket_path: PathBuf,
    store_directory: Vec<u8>,
    root_directory: PathBuf,
}

impl NixStoreRetentionBackend {
    pub fn new(
        store_uri: impl Into<String>,
        root_directory: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        Self::new_with_store_directory(store_uri, "/nix/store", root_directory)
    }

    pub fn new_with_store_directory(
        store_uri: impl Into<String>,
        store_directory: impl AsRef<Path>,
        root_directory: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        let store_uri = store_uri.into();
        let socket_path = store_uri
            .strip_prefix("unix://")
            .filter(|path| path.starts_with('/') && !path.as_bytes().contains(&0))
            .map(PathBuf::from)
            .ok_or_else(retention_error)?;
        let store_directory = store_directory
            .as_ref()
            .as_os_str()
            .as_encoded_bytes()
            .to_vec();
        let root_directory = validate_root_directory(root_directory.into())?;
        Ok(Self {
            socket_path,
            store_directory,
            root_directory,
        })
    }

    fn retain_entries(&mut self, entries: &[RetentionEntry]) -> io::Result<Vec<RetainedPath>> {
        validate_entries(entries, &self.root_directory)?;
        let stream = UnixStream::connect(&self.socket_path).map_err(|_| retention_error())?;
        stream
            .set_read_timeout(Some(OPERATION_TIMEOUT))
            .map_err(|_| retention_error())?;
        stream
            .set_write_timeout(Some(OPERATION_TIMEOUT))
            .map_err(|_| retention_error())?;
        let mut client = nix_worker_protocol::WorkerClient::connect_with_store_directory(
            stream,
            &self.store_directory,
        )
        .map_err(|_| retention_error())?;
        let mut retained = Vec::with_capacity(entries.len());
        for entry in entries {
            client
                .add_temporary_root(entry.store_path.as_bytes())
                .map_err(|error| {
                    tracing::error!(
                        event = "gateway.store_retention.failed",
                        operation = "add-temporary-root",
                        diagnostic = %error,
                        "gateway store retention failed"
                    );
                    let _ = self.rollback(&retained);
                    retention_error()
                })?;
            let root_path = self.root_directory.join(&entry.lease_id);
            let created = match create_root(&root_path, &entry.store_path) {
                Ok(created) => created,
                Err(_) => {
                    let _ = self.rollback(&retained);
                    return Err(retention_error());
                }
            };
            let retained_path = RetainedPath {
                lease_id: entry.lease_id.clone(),
                store_path: entry.store_path.clone(),
                root_path,
                created,
            };
            retained.push(retained_path);
            if let Err(error) = client.add_indirect_root(
                retained
                    .last()
                    .expect("retained path exists")
                    .root_path
                    .as_os_str()
                    .as_encoded_bytes(),
            ) {
                tracing::error!(
                    event = "gateway.store_retention.failed",
                    operation = "add-indirect-root",
                    diagnostic = %error,
                    "gateway store retention failed"
                );
                let _ = self.rollback(&retained);
                return Err(retention_error());
            }
        }
        Ok(retained)
    }
}

impl StoreRetentionBackend for NixStoreRetentionBackend {
    fn retain(&mut self, entries: &[RetentionEntry]) -> io::Result<Vec<RetainedPath>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        self.retain_entries(entries)
    }

    fn rollback(&mut self, retained: &[RetainedPath]) -> io::Result<()> {
        rollback_paths(&self.root_directory, retained)
    }

    fn release(&mut self, released: &[ReleasedRetentionEntry]) -> io::Result<()> {
        release_paths(&self.root_directory, released)
    }
}

fn rollback_paths(root_directory: &Path, retained: &[RetainedPath]) -> io::Result<()> {
    for path in retained.iter().filter(|path| path.created).rev() {
        if path.root_path.parent() != Some(root_directory)
            || fs::read_link(&path.root_path).map_err(|_| retention_error())?
                != Path::new(&path.store_path)
        {
            return Err(retention_error());
        }
        fs::remove_file(&path.root_path).map_err(|_| retention_error())?;
    }
    Ok(())
}

fn release_paths(root_directory: &Path, released: &[ReleasedRetentionEntry]) -> io::Result<()> {
    if released.is_empty() {
        return Ok(());
    }
    let mut released = released.to_vec();
    validate_released_entries(&released, root_directory)?;
    released.sort_by(|left, right| left.lease_id.cmp(&right.lease_id));
    for entry in released {
        let root_path = root_directory.join(&entry.lease_id);
        match fs::symlink_metadata(&root_path) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    && fs::read_link(&root_path).map_err(|_| retention_error())?
                        == Path::new(&entry.store_path) =>
            {
                fs::remove_file(&root_path).map_err(|_| retention_error())?;
            }
            Ok(_) => return Err(retention_error()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(retention_error()),
        }
    }
    Ok(())
}

fn create_root(root_path: &Path, store_path: &str) -> io::Result<bool> {
    match fs::symlink_metadata(root_path) {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink()
                || fs::read_link(root_path)? != Path::new(store_path)
            {
                return Err(retention_error());
            }
            return Ok(false);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(retention_error()),
    }
    std::os::unix::fs::symlink(store_path, root_path).map_err(|_| retention_error())?;
    Ok(true)
}

fn validate_root_directory(directory: PathBuf) -> io::Result<PathBuf> {
    if !directory.is_absolute() || directory.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(retention_error());
    }
    let directory = fs::canonicalize(directory).map_err(|_| retention_error())?;
    if directory.starts_with("/nix/store") {
        return Err(retention_error());
    }
    let metadata = fs::metadata(&directory).map_err(|_| retention_error())?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o022 != 0 {
        return Err(retention_error());
    }
    let probe = directory.join(format!(".telchar-retention-probe-{}", std::process::id()));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|_| retention_error())?;
    drop(file);
    fs::remove_file(probe).map_err(|_| retention_error())?;
    Ok(directory)
}

fn validate_entries(entries: &[RetentionEntry], root_directory: &Path) -> io::Result<()> {
    if entries.len() > nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES + 1 {
        return Err(retention_error());
    }
    for entry in entries {
        if entry.lease_id.is_empty()
            || entry.lease_id.len() > crate::ipc::MAX_IPC_COMPONENT_BYTES
            || entry
                .lease_id
                .bytes()
                .any(|byte| matches!(byte, b'/' | b'\0' | b'\n' | b'\r'))
            || !valid_store_path(&entry.store_path)
            || root_directory.join(&entry.lease_id).parent() != Some(root_directory)
        {
            return Err(retention_error());
        }
    }
    Ok(())
}

fn validate_released_entries(
    released: &[ReleasedRetentionEntry],
    root_directory: &Path,
) -> io::Result<()> {
    if released.len() > nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES + 1 {
        return Err(retention_error());
    }
    let mut lease_ids = std::collections::HashSet::new();
    let mut store_paths = std::collections::HashSet::new();
    for entry in released {
        if entry.lease_id.is_empty()
            || entry.lease_id.len() > crate::ipc::MAX_IPC_COMPONENT_BYTES
            || entry
                .lease_id
                .bytes()
                .any(|byte| matches!(byte, b'/' | b'\0' | b'\n' | b'\r'))
            || !valid_store_path(&entry.store_path)
            || root_directory.join(&entry.lease_id).parent() != Some(root_directory)
            || !lease_ids.insert(&entry.lease_id)
            || !store_paths.insert(&entry.store_path)
        {
            return Err(retention_error());
        }
    }
    Ok(())
}

fn valid_store_path(path: &str) -> bool {
    const HASH_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";
    let Some(name) = path.rsplit('/').next() else {
        return false;
    };
    let bytes = name.as_bytes();
    path.len() <= nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
        && bytes.len() > 33
        && bytes[32] == b'-'
        && bytes[..32].iter().all(|byte| HASH_ALPHABET.contains(byte))
        && bytes[33..].iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
        })
}

fn retention_error() -> io::Error {
    io::Error::other("gateway store retention failed")
}
