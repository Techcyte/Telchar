//! Queries exact gateway-store path validity for stock-Nix protocol operations.

use std::collections::BTreeSet;
use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::thread;

const MAXIMUM_SUBPROCESS_OUTPUT_BYTES: usize = 64 * 1024;
const MAXIMUM_RESPONSE_ENTRIES: usize = nix_worker_protocol::MAXIMUM_QUERY_VALID_PATHS;

pub trait QueryValidPathsStore {
    fn query_valid_paths(&mut self, paths: &[Vec<u8>]) -> io::Result<Vec<Vec<u8>>>;
}

pub struct GatewayStoreQuery {
    executable: String,
    store_uri: Option<String>,
    environment: Vec<(String, String)>,
}

impl GatewayStoreQuery {
    pub fn new(
        executable: impl Into<String>,
        endpoint: crate::store::daemon::GatewayStoreEndpoint,
    ) -> Self {
        Self::with_endpoint(executable, Some(endpoint))
    }

    pub fn with_endpoint(
        executable: impl Into<String>,
        endpoint: Option<crate::store::daemon::GatewayStoreEndpoint>,
    ) -> Self {
        Self::with_endpoint_and_environment(executable, endpoint, [])
    }

    pub fn with_endpoint_and_environment(
        executable: impl Into<String>,
        endpoint: Option<crate::store::daemon::GatewayStoreEndpoint>,
        environment: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        Self {
            executable: executable.into(),
            store_uri: endpoint.map(|endpoint| endpoint.to_string()),
            environment: environment.into_iter().collect(),
        }
    }

    pub fn from_environment() -> Self {
        Self {
            executable: std::env::var("TELCHAR_NIX").unwrap_or_else(|_| "nix".to_owned()),
            store_uri: std::env::var("TELCHAR_GATEWAY_STORE_URI").ok(),
            environment: Vec::new(),
        }
    }
}

impl QueryValidPathsStore for GatewayStoreQuery {
    fn query_valid_paths(&mut self, paths: &[Vec<u8>]) -> io::Result<Vec<Vec<u8>>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let store_uri = self.store_uri.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "gateway store endpoint is not configured",
            )
        })?;
        let requested = paths
            .iter()
            .map(|path| String::from_utf8(path.clone()).map_err(|_| invalid_response()))
            .collect::<io::Result<BTreeSet<_>>>()?;
        let mut command = Command::new(&self.executable);
        command
            .args([
                "--extra-experimental-features",
                "nix-command",
                "path-info",
                "--store",
                store_uri,
                "--json",
            ])
            .args(
                paths
                    .iter()
                    .map(|path| String::from_utf8_lossy(path).into_owned()),
            )
            .envs(
                self.environment
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = run_bounded(command)?;
        if output.exceeded_limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "path-info response exceeds limit",
            ));
        }
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "gateway store query failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let entries: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&output.stdout).map_err(|_| invalid_response())?;
        if entries.len() > MAXIMUM_RESPONSE_ENTRIES {
            return Err(invalid_response());
        }
        let mut valid = BTreeSet::new();
        for (key, value) in entries {
            if !requested.contains(&key) {
                return Err(invalid_response());
            }
            match value {
                serde_json::Value::Null => {}
                serde_json::Value::Object(_) => {
                    valid.insert(key);
                }
                _ => return Err(invalid_response()),
            }
        }
        Ok(valid.into_iter().map(String::into_bytes).collect())
    }
}

struct BoundedOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exceeded_limit: bool,
}

fn run_bounded(mut command: Command) -> io::Result<BoundedOutput> {
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("stderr unavailable"))?;
    let stdout_reader = thread::spawn(|| drain(stdout));
    let stderr_reader = thread::spawn(|| drain(stderr));
    let status = child.wait()?;
    let (stdout, stdout_exceeded) = stdout_reader
        .join()
        .map_err(|_| io::Error::other("stdout reader panicked"))??;
    let (stderr, stderr_exceeded) = stderr_reader
        .join()
        .map_err(|_| io::Error::other("stderr reader panicked"))??;
    Ok(BoundedOutput {
        status,
        stdout,
        stderr,
        exceeded_limit: stdout_exceeded || stderr_exceeded,
    })
}

fn drain(mut source: impl Read) -> io::Result<(Vec<u8>, bool)> {
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

fn invalid_response() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid gateway store response")
}
