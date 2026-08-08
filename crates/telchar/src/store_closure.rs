use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MAXIMUM_RESPONSE_BYTES: usize = 1024 * 1024;
const MAXIMUM_DIAGNOSTIC_BYTES: usize = 4096;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, serde::Serialize)]
struct ClosureRequest<'a> {
    version: u32,
    store_uri: &'a str,
    roots: Vec<&'a str>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosureResponse {
    version: u32,
    paths: Vec<String>,
}

pub trait StoreClosureBackend: Send {
    fn input_closure(&mut self, roots: &[Vec<u8>]) -> io::Result<Vec<String>>;
}

pub fn backend_from_environment() -> io::Result<Box<dyn StoreClosureBackend>> {
    let Some(helper) = std::env::var_os("TELCHAR_NIX_STORE_CLOSURE") else {
        return Ok(Box::new(UnavailableStoreClosureBackend));
    };
    let Some(store_uri) = std::env::var_os("TELCHAR_GATEWAY_STORE_URI") else {
        return Ok(Box::new(UnavailableStoreClosureBackend));
    };
    Ok(Box::new(NixStoreClosureBackend::new(
        helper,
        store_uri.to_string_lossy(),
    )?))
}

struct UnavailableStoreClosureBackend;

impl StoreClosureBackend for UnavailableStoreClosureBackend {
    fn input_closure(&mut self, roots: &[Vec<u8>]) -> io::Result<Vec<String>> {
        if roots.is_empty() {
            Ok(Vec::new())
        } else {
            Err(query_error())
        }
    }
}

pub struct NixStoreClosureBackend {
    helper: PathBuf,
    store_uri: String,
}

impl NixStoreClosureBackend {
    pub fn new(helper: impl Into<PathBuf>, store_uri: impl Into<String>) -> io::Result<Self> {
        let helper = helper.into();
        if !helper.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "store closure helper path is not absolute",
            ));
        }
        let store_uri = store_uri.into();
        if store_uri.is_empty() || store_uri.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "gateway store endpoint is not configured",
            ));
        }
        Ok(Self { helper, store_uri })
    }

    fn query(&mut self, roots: &[Vec<u8>]) -> io::Result<Vec<String>> {
        let root_strings = roots
            .iter()
            .map(|root| {
                let root = std::str::from_utf8(root).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "input closure query failed")
                })?;
                if root.len() > nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
                    || !root.starts_with("/nix/store/")
                    || root
                        .bytes()
                        .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r'))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "input closure query failed",
                    ));
                }
                Ok(root)
            })
            .collect::<io::Result<Vec<_>>>()?;
        let payload = serde_json::to_vec(&ClosureRequest {
            version: 1,
            store_uri: &self.store_uri,
            roots: root_strings.clone(),
        })
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "input closure query failed"))?;
        if payload.len() > MAXIMUM_RESPONSE_BYTES {
            return Err(query_error());
        }

        let mut command = Command::new(&self.helper);
        configure_child_lifecycle(&mut command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().map_err(|_| query_error())?;
        let mut child = ChildGuard::new(child);
        let mut stdin = child.child.stdin.take().ok_or_else(query_error)?;
        let stdout = child.child.stdout.take().ok_or_else(query_error)?;
        let stderr = child.child.stderr.take().ok_or_else(query_error)?;
        let stdout_reader = thread::spawn(|| drain_bounded(stdout, MAXIMUM_RESPONSE_BYTES));
        let stderr_reader = thread::spawn(|| drain_bounded(stderr, MAXIMUM_DIAGNOSTIC_BYTES));
        let write_result = stdin.write_all(&payload).and_then(|_| stdin.flush());
        drop(stdin);
        if write_result.is_err() {
            child.kill_and_reap();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(query_error());
        }
        let deadline = Instant::now() + OPERATION_TIMEOUT;
        let status = loop {
            match child.child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    child.kill_and_reap();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(query_error());
                }
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                Err(_) => {
                    child.kill_and_reap();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(query_error());
                }
            }
        };
        let (stdout, stdout_overflow) = stdout_reader.join().map_err(|_| query_error())??;
        let (_stderr, stderr_overflow) = stderr_reader.join().map_err(|_| query_error())??;
        if !status.success()
            || stdout_overflow
            || stderr_overflow
            || stdout.len() > MAXIMUM_RESPONSE_BYTES
        {
            return Err(query_error());
        }
        let response: ClosureResponse =
            serde_json::from_slice(&stdout).map_err(|_| query_error())?;
        if response.version != 1
            || response.paths.len() > nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES
        {
            return Err(query_error());
        }
        let paths = normalize_paths(response.paths)?;
        if root_strings
            .iter()
            .any(|root| !paths.iter().any(|path| path == root))
        {
            return Err(query_error());
        }
        Ok(paths)
    }
}

impl StoreClosureBackend for NixStoreClosureBackend {
    fn input_closure(&mut self, roots: &[Vec<u8>]) -> io::Result<Vec<String>> {
        if roots.is_empty() {
            return Ok(Vec::new());
        }
        if roots.len() > nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES {
            return Err(query_error());
        }
        self.query(roots)
    }
}

fn normalize_paths(paths: Vec<String>) -> io::Result<Vec<String>> {
    let mut paths = paths;
    for path in &paths {
        if path.len() > nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
            || !path.starts_with("/nix/store/")
            || path
                .bytes()
                .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r'))
        {
            return Err(query_error());
        }
    }
    paths.sort_unstable();
    paths.dedup();
    if paths.len() > nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES {
        return Err(query_error());
    }
    Ok(paths)
}

fn query_error() -> io::Error {
    io::Error::other("input closure query failed")
}

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child }
    }

    fn kill_and_reap(&mut self) {
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
    command.process_group(0);
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn configure_child_lifecycle(_command: &mut Command) {}
