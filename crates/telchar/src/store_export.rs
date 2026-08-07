use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine;

use crate::nar::stage_nar;
use crate::store_promotion::RegisteredPathInfo;

const MAXIMUM_SUBPROCESS_OUTPUT_BYTES: usize = 64 * 1024;

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

pub trait StoreExportBackend: Send {
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

    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo> {
        let mut command = std::process::Command::new("nix");
        command
            .envs(self.environment.iter().cloned())
            .args(["--store", &self.store_uri, "path-info", "--json"])
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = run_bounded_command(command)?;
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

    fn export_nar(&mut self, request: &StoreExportRequest, sink: &mut dyn Write) -> io::Result<()> {
        let mut command = std::process::Command::new(&self.helper);
        command
            .envs(self.environment.iter().cloned())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("export helper stdin not configured"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("export helper stdout not configured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("export helper stderr not configured"))?;
        let stderr_reader = std::thread::spawn(move || drain_bounded(stderr));
        let request_result = serde_json::to_writer(&mut stdin, request)
            .map_err(io::Error::other)
            .and_then(|_| stdin.flush());
        if let Err(error) = request_result {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_reader.join();
            return Err(error);
        }
        drop(stdin);

        if let Err(error) = io::copy(&mut stdout, sink).map(|_| ()) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_reader.join();
            return Err(error);
        }
        let status = child.wait()?;
        let (_, exceeded) = stderr_reader
            .join()
            .map_err(|_| io::Error::other("export helper stderr reader panicked"))??;
        if !status.success() || exceeded {
            return Err(io::Error::other("export helper failed"));
        }
        Ok(())
    }
}

pub fn export_verified_nar(
    path: &Path,
    sink: &mut impl Write,
    backend: &mut impl StoreExportBackend,
) -> io::Result<VerifiedStoreExport> {
    let metadata = backend
        .query_path_info(path)
        .map_err(|error| io::Error::other(format!("path info query failed: {error}")))?;
    let request = StoreExportRequest {
        version: 1,
        store_uri: backend.store_uri().to_owned(),
        path: path.to_path_buf(),
    };
    let (sender, receiver) = std::sync::mpsc::sync_channel(0);
    let reader = ExportReader {
        receiver,
        pending: None,
        offset: 0,
    };
    let mut writer = ExportWriter { sender };
    let fingerprint = std::thread::scope(|scope| {
        let export = scope.spawn(move || backend.export_nar(&request, &mut writer));
        let parsed = stage_nar(reader, sink);
        let exported = export
            .join()
            .map_err(|_| io::Error::other("export backend thread panicked"))?;
        match parsed {
            Err(error) => Err(error),
            Ok(fingerprint) => {
                exported?;
                Ok(fingerprint)
            }
        }
    })?;
    if fingerprint.sha256 != metadata.nar_hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exported NAR hash mismatch",
        ));
    }
    if fingerprint.size != metadata.nar_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exported NAR size mismatch",
        ));
    }
    Ok(VerifiedStoreExport {
        metadata,
        nar_hash: fingerprint.sha256,
        nar_size: fingerprint.size,
    })
}

struct ExportMessage {
    bytes: Vec<u8>,
    acknowledgement: std::sync::mpsc::SyncSender<io::Result<()>>,
}

struct ExportReader {
    receiver: std::sync::mpsc::Receiver<ExportMessage>,
    pending: Option<ExportMessage>,
    offset: usize,
}

impl Read for ExportReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.pending.is_none() {
            self.pending = match self.receiver.recv() {
                Ok(message) => Some(message),
                Err(_) => return Ok(0),
            };
            self.offset = 0;
        }
        let message = self.pending.as_ref().expect("pending export message");
        buffer[0] = message.bytes[self.offset];
        self.offset += 1;
        message
            .acknowledgement
            .send(Ok(()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "export parser stopped"))?;
        if self.offset == message.bytes.len() {
            self.pending.take();
        }
        Ok(1)
    }
}

impl Drop for ExportReader {
    fn drop(&mut self) {
        if let Some(message) = self.pending.take() {
            let _ = message.acknowledgement.send(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "export parser stopped",
            )));
        }
    }
}

struct ExportWriter {
    sender: std::sync::mpsc::SyncSender<ExportMessage>,
}

impl Write for ExportWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let (acknowledgement, result) = std::sync::mpsc::sync_channel(buffer.len());
        self.sender
            .send(ExportMessage {
                bytes: buffer.to_vec(),
                acknowledgement,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "export parser stopped"))?;
        for _ in buffer {
            result.recv().map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "export parser stopped")
            })??;
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
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

fn parse_sha256_sri(value: &str) -> io::Result<[u8; 32]> {
    let encoded = value
        .strip_prefix("sha256-")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unsupported NAR hash"))?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid NAR hash"))?
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid NAR hash length"))
}

struct BoundedCommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    exceeded_limit: bool,
}

fn run_bounded_command(mut command: std::process::Command) -> io::Result<BoundedCommandOutput> {
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
    let status = child.wait()?;
    let (stdout, stdout_exceeded) = stdout_reader
        .join()
        .map_err(|_| io::Error::other("subprocess stdout reader panicked"))??;
    let (_, stderr_exceeded) = stderr_reader
        .join()
        .map_err(|_| io::Error::other("subprocess stderr reader panicked"))??;
    Ok(BoundedCommandOutput {
        status,
        stdout,
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
