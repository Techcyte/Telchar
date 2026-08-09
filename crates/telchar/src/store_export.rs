use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine;

use crate::nar::stage_nar;
use crate::store_daemon::{GatewayStoreConnection, GatewayStoreEndpoint};
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
    fn export_nar(
        &mut self,
        request: &StoreExportRequest,
        nar_size: u64,
        sink: &mut dyn Write,
    ) -> io::Result<()>;
}

pub struct UnavailableStoreExportBackend;

impl StoreExportBackend for UnavailableStoreExportBackend {
    fn store_uri(&self) -> &str {
        ""
    }

    fn query_path_info(&mut self, _path: &Path) -> io::Result<RegisteredPathInfo> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "store export is unavailable",
        ))
    }

    fn export_nar(
        &mut self,
        _request: &StoreExportRequest,
        _nar_size: u64,
        _sink: &mut dyn Write,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "store export is unavailable",
        ))
    }
}

pub fn backend_from_environment() -> io::Result<Box<dyn StoreExportBackend>> {
    if let Some(helper) = std::env::var_os("TELCHAR_TEST_EXPORT_HELPER") {
        let store_uri = std::env::var("TELCHAR_GATEWAY_STORE_URI").map_err(|_| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "gateway store endpoint is not configured",
            )
        })?;
        let nix = std::env::var_os("TELCHAR_NIX").ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "Nix executable is not configured")
        })?;
        return Ok(Box::new(NixStoreExportBackend::new(
            helper,
            store_uri,
            [("TELCHAR_NIX".to_owned(), nix.to_string_lossy().into_owned())],
        )));
    }
    let Some(value) = std::env::var_os("TELCHAR_GATEWAY_STORE_URI") else {
        return Ok(Box::new(UnavailableStoreExportBackend));
    };
    let endpoint = GatewayStoreEndpoint::parse_os(&value)?;
    Ok(Box::new(GatewayStoreExportBackend::new(endpoint)))
}

pub struct GatewayStoreExportBackend {
    endpoint: GatewayStoreEndpoint,
}

impl GatewayStoreExportBackend {
    pub fn new(endpoint: GatewayStoreEndpoint) -> Self {
        Self { endpoint }
    }
}

impl StoreExportBackend for GatewayStoreExportBackend {
    fn store_uri(&self) -> &str {
        "configured-gateway-daemon"
    }

    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo> {
        let mut connection = GatewayStoreConnection::connect(&self.endpoint)?;
        let info = connection
            .query_path_info(path.as_os_str().as_encoded_bytes())?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "registered path omitted"))?;
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

    fn export_nar(
        &mut self,
        request: &StoreExportRequest,
        nar_size: u64,
        sink: &mut dyn Write,
    ) -> io::Result<()> {
        let mut connection = GatewayStoreConnection::connect(&self.endpoint)?;
        connection.nar_from_path(request.path.as_os_str().as_encoded_bytes(), nar_size, sink)
    }
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
        let nix = self
            .environment
            .iter()
            .find_map(|(name, value)| (name == "TELCHAR_NIX").then_some(value.as_str()))
            .unwrap_or("nix");
        let mut command = std::process::Command::new(nix);
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
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = run_bounded_command(command)?;
        if output.exceeded_limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "registered path-info query output exceeds limit",
            ));
        }
        if !output.status.success() {
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            let kind = if diagnostic.contains("is not valid")
                || diagnostic.contains("path-info command failed")
                || diagnostic.contains("No such file or directory")
            {
                io::ErrorKind::NotFound
            } else {
                io::ErrorKind::Other
            };
            return Err(io::Error::new(kind, diagnostic.trim().to_owned()));
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

    fn export_nar(
        &mut self,
        request: &StoreExportRequest,
        _nar_size: u64,
        sink: &mut dyn Write,
    ) -> io::Result<()> {
        let mut command = std::process::Command::new(&self.helper);
        configure_child_lifecycle(&mut command);
        command
            .envs(self.environment.iter().cloned())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = command.spawn()?;
        let mut child = ExportChild::new(child);
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
            child.kill_and_reap();
            let _ = stderr_reader.join();
            return Err(error);
        }
        drop(stdin);

        if let Err(error) = io::copy(&mut stdout, sink).map(|_| ()) {
            child.kill_and_reap();
            let _ = stderr_reader.join();
            return Err(error);
        }
        let status = child.wait()?;
        let (_, exceeded) = stderr_reader
            .join()
            .map_err(|_| io::Error::other("export helper stderr reader panicked"))??;
        if exceeded {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "export helper output exceeds limit",
            ));
        }
        if !status.success() {
            return Err(io::Error::other("export helper failed"));
        }
        Ok(())
    }
}

struct ExportChild {
    child: Option<std::process::Child>,
}

impl ExportChild {
    fn new(child: std::process::Child) -> Self {
        Self { child: Some(child) }
    }

    fn kill_and_reap(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        let mut child = self
            .child
            .take()
            .ok_or_else(|| io::Error::other("export helper already reaped"))?;
        child.wait()
    }
}

impl std::ops::Deref for ExportChild {
    type Target = std::process::Child;

    fn deref(&self) -> &Self::Target {
        self.child.as_ref().expect("export helper available")
    }
}

impl std::ops::DerefMut for ExportChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.child.as_mut().expect("export helper available")
    }
}

impl Drop for ExportChild {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

pub fn query_path_info(
    path: &Path,
    backend: &mut (impl StoreExportBackend + ?Sized),
) -> io::Result<Option<RegisteredPathInfo>> {
    match backend.query_path_info(path) {
        Ok(info) => Ok(Some(info)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn export_verified_nar(
    path: &Path,
    sink: &mut impl Write,
    backend: &mut (impl StoreExportBackend + ?Sized),
) -> io::Result<VerifiedStoreExport> {
    let limits = crate::transfer_limits::TransferLimits::from_environment()?;
    let mut session =
        crate::transfer_limits::TransferBudget::new(limits.maximum_outbound_session_bytes);
    let rates = crate::transfer_limits::RateAdmissionState::new(&limits);
    export_verified_nar_with_limits_and_rate(path, sink, backend, &limits, &mut session, &rates)
}

pub fn export_verified_nar_with_limits(
    path: &Path,
    sink: &mut impl Write,
    backend: &mut (impl StoreExportBackend + ?Sized),
    limits: &crate::transfer_limits::TransferLimits,
    session: &mut crate::transfer_limits::TransferBudget,
) -> io::Result<VerifiedStoreExport> {
    let rates = crate::transfer_limits::RateAdmissionState::new(limits);
    export_verified_nar_with_limits_and_rate(path, sink, backend, limits, session, &rates)
}

pub fn export_verified_nar_with_limits_and_rate(
    path: &Path,
    sink: &mut impl Write,
    backend: &mut (impl StoreExportBackend + ?Sized),
    limits: &crate::transfer_limits::TransferLimits,
    session: &mut crate::transfer_limits::TransferBudget,
    rates: &crate::transfer_limits::RateAdmissionState,
) -> io::Result<VerifiedStoreExport> {
    let mut limited_sink = crate::transfer_limits::LimitedWriter::with_rate(
        sink,
        limits.maximum_object_bytes,
        session,
        rates.clone(),
    );
    verify_exported_nar(path, &mut limited_sink, backend)
}

pub fn validate_store_output(
    path: &Path,
    backend: &mut (impl StoreExportBackend + ?Sized),
) -> io::Result<RegisteredPathInfo> {
    let mut sink = io::sink();
    verify_exported_nar(path, &mut sink, backend).map(|verified| verified.metadata)
}

fn verify_exported_nar(
    path: &Path,
    sink: &mut impl Write,
    backend: &mut (impl StoreExportBackend + ?Sized),
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
        let export =
            scope.spawn(move || backend.export_nar(&request, metadata.nar_size, &mut writer));
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
        if self
            .pending
            .as_ref()
            .is_some_and(|message| self.offset == message.bytes.len())
        {
            let message = self
                .pending
                .take()
                .ok_or_else(|| io::Error::other("export message missing"))?;
            message
                .acknowledgement
                .send(Ok(()))
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "export parser stopped"))?;
        }
        if self.pending.is_none() {
            self.pending = match self.receiver.recv() {
                Ok(message) => Some(message),
                Err(_) => return Ok(0),
            };
            self.offset = 0;
        }
        let message = self
            .pending
            .as_ref()
            .ok_or_else(|| io::Error::other("export message missing"))?;
        let remaining = &message.bytes[self.offset..];
        let read = remaining.len().min(buffer.len());
        buffer[..read].copy_from_slice(&remaining[..read]);
        self.offset += read;
        Ok(read)
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
        let (acknowledgement, result) = std::sync::mpsc::sync_channel(1);
        self.sender
            .send(ExportMessage {
                bytes: buffer.to_vec(),
                acknowledgement,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "export parser stopped"))?;
        result
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "export parser stopped"))??;
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

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn configure_child_lifecycle(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
                .map_err(io::Error::from)?;
            if rustix::process::getppid() == Some(rustix::process::Pid::INIT) {
                return Err(io::Error::other("export owner exited before helper start"));
            }
            Ok(())
        });
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn configure_child_lifecycle(_command: &mut std::process::Command) {}

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
    stderr: Vec<u8>,
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
