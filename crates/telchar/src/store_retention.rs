use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MAXIMUM_MESSAGE_BYTES: usize = 1024 * 1024;
const MAXIMUM_DIAGNOSTIC_BYTES: usize = 4096;
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

pub trait StoreRetentionBackend: Send {
    fn retain(&mut self, entries: &[RetentionEntry]) -> io::Result<Vec<RetainedPath>>;
    fn rollback(&mut self, retained: &[RetainedPath]) -> io::Result<()>;
}

#[derive(serde::Serialize)]
struct RetentionRequest<'a> {
    version: u32,
    store_uri: &'a str,
    root_directory: &'a Path,
    entries: &'a [RetentionEntry],
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionResponse {
    version: u32,
    retained: Vec<RetentionResponseEntry>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionResponseEntry {
    lease_id: String,
    store_path: String,
    root_path: PathBuf,
}

impl serde::Serialize for RetentionEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("RetentionEntry", 2)?;
        state.serialize_field("lease_id", &self.lease_id)?;
        state.serialize_field("store_path", &self.store_path)?;
        state.end()
    }
}

pub fn backend_from_environment() -> io::Result<Box<dyn StoreRetentionBackend>> {
    let Some(helper) = std::env::var_os("TELCHAR_NIX_STORE_RETAIN") else {
        return Ok(Box::new(UnavailableStoreRetentionBackend));
    };
    let Some(root_directory) = std::env::var_os("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY") else {
        return Ok(Box::new(UnavailableStoreRetentionBackend));
    };
    let Ok(store_uri) = std::env::var("TELCHAR_GATEWAY_STORE_URI") else {
        return Ok(Box::new(UnavailableStoreRetentionBackend));
    };
    Ok(Box::new(NixStoreRetentionBackend::new(
        helper,
        store_uri,
        root_directory,
    )?))
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
}

pub struct NixStoreRetentionBackend {
    helper: PathBuf,
    store_uri: String,
    root_directory: PathBuf,
}

impl NixStoreRetentionBackend {
    pub fn new(
        helper: impl Into<PathBuf>,
        store_uri: impl Into<String>,
        root_directory: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        let helper = helper.into();
        if !helper.is_absolute() {
            return Err(retention_error());
        }
        let store_uri = store_uri.into();
        if !store_uri.starts_with("unix://") || store_uri.contains('\0') {
            return Err(retention_error());
        }
        let root_directory = validate_root_directory(root_directory.into())?;
        Ok(Self {
            helper,
            store_uri,
            root_directory,
        })
    }

    fn retain_entries(&mut self, entries: &[RetentionEntry]) -> io::Result<Vec<RetainedPath>> {
        validate_entries(entries, &self.root_directory)?;
        let created = entries
            .iter()
            .map(|entry| !self.root_directory.join(&entry.lease_id).exists())
            .collect::<Vec<_>>();
        let payload = serde_json::to_vec(&RetentionRequest {
            version: 1,
            store_uri: &self.store_uri,
            root_directory: &self.root_directory,
            entries,
        })
        .map_err(|_| retention_error())?;
        if payload.len() > MAXIMUM_MESSAGE_BYTES {
            return Err(retention_error());
        }
        let output = run_helper(&self.helper, &payload)?;
        let response: RetentionResponse =
            serde_json::from_slice(&output).map_err(|_| retention_error())?;
        if response.version != 1 || response.retained.len() != entries.len() {
            return Err(retention_error());
        }
        response
            .retained
            .into_iter()
            .zip(entries.iter().zip(created))
            .map(|(returned, (expected, created))| {
                let root_path = self.root_directory.join(&expected.lease_id);
                if returned.lease_id != expected.lease_id
                    || returned.store_path != expected.store_path
                    || returned.root_path != root_path
                {
                    return Err(retention_error());
                }
                Ok(RetainedPath {
                    lease_id: expected.lease_id.clone(),
                    store_path: expected.store_path.clone(),
                    root_path,
                    created,
                })
            })
            .collect()
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
        for path in retained.iter().filter(|path| path.created) {
            if path.root_path.parent() != Some(self.root_directory.as_path())
                || fs::read_link(&path.root_path).map_err(|_| retention_error())?
                    != Path::new(&path.store_path)
            {
                return Err(retention_error());
            }
            fs::remove_file(&path.root_path).map_err(|_| retention_error())?;
        }
        Ok(())
    }
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

fn valid_store_path(path: &str) -> bool {
    const HASH_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";
    let Some(name) = path.strip_prefix("/nix/store/") else {
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

fn run_helper(helper: &Path, payload: &[u8]) -> io::Result<Vec<u8>> {
    let mut command = Command::new(helper);
    configure_child_lifecycle(&mut command);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().map_err(|_| retention_error())?;
    let mut child = ChildGuard::new(child);
    let mut stdin = child.child.stdin.take().ok_or_else(retention_error)?;
    let stdout = child.child.stdout.take().ok_or_else(retention_error)?;
    let stderr = child.child.stderr.take().ok_or_else(retention_error)?;
    let stdout_reader = thread::spawn(|| drain_bounded(stdout, MAXIMUM_MESSAGE_BYTES));
    let stderr_reader = thread::spawn(|| drain_bounded(stderr, MAXIMUM_DIAGNOSTIC_BYTES));
    if stdin
        .write_all(payload)
        .and_then(|_| stdin.flush())
        .is_err()
    {
        child.kill_and_reap();
        let _ = stdout_reader.join();
        let _ = stderr_reader.join();
        return Err(retention_error());
    }
    drop(stdin);
    let deadline = Instant::now() + OPERATION_TIMEOUT;
    let status = loop {
        match child.child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                child.kill_and_reap();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(retention_error());
            }
        }
    };
    let (stdout, stdout_overflow) = stdout_reader.join().map_err(|_| retention_error())??;
    let (_, stderr_overflow) = stderr_reader.join().map_err(|_| retention_error())??;
    if !status.success() || stdout_overflow || stderr_overflow {
        return Err(retention_error());
    }
    Ok(stdout)
}

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child }
    }

    fn kill_and_reap(&mut self) {
        if let Some(pid) =
            rustix::process::Pid::from_raw(self.child.id() as rustix::process::RawPid)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

fn drain_bounded(mut source: impl Read, maximum: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    let mut overflow = false;
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if output.len().saturating_add(count) > maximum {
            overflow = true;
        } else {
            output.extend_from_slice(&buffer[..count]);
        }
    }
    Ok((output, overflow))
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn configure_child_lifecycle(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
                .map_err(io::Error::from)?;
            rustix::process::setpgid(None, None).map_err(io::Error::from)?;
            if rustix::process::getppid() == Some(rustix::process::Pid::INIT) {
                return Err(io::Error::other(
                    "retention owner exited before helper start",
                ));
            }
            Ok(())
        });
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn configure_child_lifecycle(_: &mut Command) {}

fn retention_error() -> io::Error {
    io::Error::other("gateway store retention failed")
}
