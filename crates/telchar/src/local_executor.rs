//! Runs local Nix BuildDerivation work through a fixed helper while bounding logs, results, cancellation, and timeouts.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix_worker_protocol::{
    BuildDerivationClientRequest, BuildDerivationOutputRequest, WorkerBuildStatus,
};

use crate::backend::{BuildBackend, BuildExecution, BuildResult, BuildStatus, OutputTrust};
use crate::build_request::BuildRequest;
use crate::store_daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

const MAXIMUM_SUBPROCESS_OUTPUT_BYTES: usize = 64 * 1024;
const MAXIMUM_BUILD_LOG_CHUNK_BYTES: usize = 8192;
const MAXIMUM_QUEUED_BUILD_LOG_CHUNKS: usize = 8;
const MAXIMUM_QUEUED_BUILD_LOG_PAYLOAD_BYTES: usize =
    MAXIMUM_BUILD_LOG_CHUNK_BYTES * MAXIMUM_QUEUED_BUILD_LOG_CHUNKS;

pub fn executor_from_environment() -> io::Result<Box<dyn BuildBackend>> {
    if let Some(helper) = std::env::var_os("TELCHAR_TEST_BUILD_HELPER") {
        return Ok(Box::new(NixStoreExecutor::new(
            PathBuf::from(helper),
            std::env::var("TELCHAR_GATEWAY_STORE_URI").map_err(|_| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "gateway store endpoint is not configured",
                )
            })?,
        )?));
    }
    let Some(value) = std::env::var_os("TELCHAR_GATEWAY_STORE_URI") else {
        return Ok(Box::new(UnavailableBuildExecutor));
    };
    let endpoint = GatewayStoreEndpoint::parse_os(&value)?;
    Ok(Box::new(GatewayStoreExecutor::new(endpoint)))
}

pub struct UnavailableBuildExecutor;

impl BuildBackend for UnavailableBuildExecutor {
    fn execute_with_logs(
        &mut self,
        _request: &BuildExecution<'_>,
        _logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        _cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "BuildDerivation execution is unavailable",
        ))
    }
}

pub struct GatewayStoreExecutor {
    endpoint: GatewayStoreEndpoint,
}

impl GatewayStoreExecutor {
    pub fn new(endpoint: GatewayStoreEndpoint) -> Self {
        Self { endpoint }
    }

    fn execute_request(
        &mut self,
        request: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        let mut connection =
            GatewayStoreConnection::connect_with_timeout(&self.endpoint, request.timeout())?;
        let shutdown = connection.shutdown_handle()?;
        let outputs = request
            .build()
            .expected_outputs()
            .iter()
            .map(|(name, path)| BuildDerivationOutputRequest {
                name: name.as_slice(),
                path: path.as_slice(),
            })
            .collect::<Vec<_>>();
        let build = request.build();
        let daemon_request = BuildDerivationClientRequest {
            drv_path: build.derivation_path(),
            outputs: &outputs,
            input_sources: build.input_sources(),
            platform: build.system().as_bytes(),
            builder: build.builder(),
            arguments: build.arguments(),
            environment: build.environment(),
        };
        debug_assert_eq!(
            MAXIMUM_QUEUED_BUILD_LOG_PAYLOAD_BYTES,
            MAXIMUM_BUILD_LOG_CHUNK_BYTES * MAXIMUM_QUEUED_BUILD_LOG_CHUNKS
        );
        let (log_sender, log_receiver) =
            std::sync::mpsc::sync_channel(MAXIMUM_QUEUED_BUILD_LOG_CHUNKS);
        let (result_sender, result_receiver) = std::sync::mpsc::sync_channel(1);
        let deadline = Instant::now() + request.timeout();
        let result = std::thread::scope(|scope| {
            let worker = scope.spawn(move || {
                let result = connection.build_derivation(&daemon_request, &mut |message| {
                    for chunk in message.chunks(MAXIMUM_BUILD_LOG_CHUNK_BYTES) {
                        log_sender.send(chunk.to_vec()).map_err(|_| {
                            io::Error::new(io::ErrorKind::BrokenPipe, "build log receiver closed")
                        })?;
                    }
                    Ok(())
                });
                let _ = result_sender.send(result);
            });
            loop {
                if let Err(error) = forward_logs(&log_receiver, logs) {
                    let _ = shutdown.shutdown(std::net::Shutdown::Both);
                    worker
                        .join()
                        .map_err(|_| io::Error::other("build worker failed"))?;
                    return Err(error);
                }
                if let Ok(result) = result_receiver.try_recv() {
                    worker
                        .join()
                        .map_err(|_| io::Error::other("build worker failed"))?;
                    forward_logs(&log_receiver, logs)?;
                    break result;
                }
                if cancelled()? {
                    let _ = shutdown.shutdown(std::net::Shutdown::Both);
                    worker
                        .join()
                        .map_err(|_| io::Error::other("build worker failed"))?;
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "build requester disconnected",
                    ));
                }
                if Instant::now() >= deadline {
                    let _ = shutdown.shutdown(std::net::Shutdown::Both);
                    worker
                        .join()
                        .map_err(|_| io::Error::other("build worker failed"))?;
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "build timed out"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })?;
        let mut actual_outputs = result.outputs().to_vec();
        actual_outputs.sort();
        let mut expected_outputs = build.expected_outputs().to_vec();
        expected_outputs.sort();
        if actual_outputs != expected_outputs {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "build output set mismatch",
            ));
        }
        for (_, path) in &expected_outputs {
            let mut verification = GatewayStoreConnection::connect(&self.endpoint)?;
            if verification.query_path_info(path)?.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "build output verification failed",
                ));
            }
        }
        BuildResult::new(
            match result.status() {
                WorkerBuildStatus::Built => BuildStatus::Built,
                WorkerBuildStatus::AlreadyValid => BuildStatus::AlreadyValid,
            },
            build.expected_outputs().to_vec(),
            OutputTrust::TrustedExecutor,
        )
    }
}

impl BuildBackend for GatewayStoreExecutor {
    fn execute_with_logs(
        &mut self,
        request: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        self.execute_request(request, logs, cancelled)
    }
}

pub struct NixStoreExecutor {
    helper: PathBuf,
    store_uri: String,
}

impl NixStoreExecutor {
    pub fn new(helper: impl Into<PathBuf>, store_uri: impl Into<String>) -> io::Result<Self> {
        let helper = helper.into();
        if !helper.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "local executor helper path is not absolute",
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

    pub fn helper(&self) -> &Path {
        &self.helper
    }

    pub fn store_uri(&self) -> &str {
        &self.store_uri
    }

    fn execute_request(
        &mut self,
        request: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        let payload = encode_request(request)?;
        let mut command = std::process::Command::new(&self.helper);
        configure_child_lifecycle(&mut command);
        command
            .arg(&self.store_uri)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = command.spawn()?;
        let mut child = ChildGuard::new(child);
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("build helper stdin not configured"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("build helper stdout not configured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("build helper stderr not configured"))?;
        let (reader_error_sender, reader_error_receiver) = std::sync::mpsc::channel();
        let stdout_reader = spawn_reader(stdout, reader_error_sender.clone());
        debug_assert_eq!(
            MAXIMUM_QUEUED_BUILD_LOG_PAYLOAD_BYTES,
            MAXIMUM_BUILD_LOG_CHUNK_BYTES * MAXIMUM_QUEUED_BUILD_LOG_CHUNKS
        );
        let (log_sender, log_receiver) =
            std::sync::mpsc::sync_channel(MAXIMUM_QUEUED_BUILD_LOG_CHUNKS);
        let stderr_reader = spawn_log_reader(stderr, log_sender, reader_error_sender);

        let write_result = stdin.write_all(&payload).and_then(|_| stdin.flush());
        drop(stdin);
        if let Err(error) = write_result {
            child.kill_and_reap();
            drop(log_receiver);
            join_reader(stdout_reader, "stdout")?;
            join_log_reader(stderr_reader)?;
            return Err(error);
        }

        let deadline = Instant::now() + request.timeout();
        let status = loop {
            if let Ok(error) = reader_error_receiver.try_recv() {
                child.kill_and_reap();
                drop(log_receiver);
                let _ = join_reader(stdout_reader, "stdout");
                let _ = join_log_reader(stderr_reader);
                return Err(error);
            }
            if let Err(error) = forward_logs(&log_receiver, logs) {
                child.kill_and_reap();
                drop(log_receiver);
                let _ = join_reader(stdout_reader, "stdout");
                let _ = join_log_reader(stderr_reader);
                return Err(error);
            }
            if cancelled()? {
                child.kill_and_reap();
                drop(log_receiver);
                let _ = join_reader(stdout_reader, "stdout");
                let _ = join_log_reader(stderr_reader);
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "build requester disconnected",
                ));
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Err(error) => {
                    child.kill_and_reap();
                    drop(log_receiver);
                    let _ = join_reader(stdout_reader, "stdout");
                    let _ = join_log_reader(stderr_reader);
                    return Err(error);
                }
                Ok(None) if Instant::now() >= deadline => {
                    child.kill_and_reap();
                    drop(log_receiver);
                    join_reader(stdout_reader, "stdout")?;
                    join_log_reader(stderr_reader)?;
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "build helper timed out",
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        let stdout = join_reader(stdout_reader, "stdout")?;
        join_log_reader(stderr_reader)?;
        forward_logs(&log_receiver, logs)?;
        if stdout.1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "build helper output exceeds limit",
            ));
        }
        if !status.success() {
            return Err(io::Error::other("build helper failed"));
        }
        parse_response(&stdout.0, request.build())
    }
}

#[derive(serde::Serialize)]
struct ExecutionRequest<'a> {
    version: u32,
    request_id: &'a str,
    derivation_path: &'a str,
    outputs: Vec<OutputRequest<'a>>,
    input_sources: Vec<&'a str>,
    system: &'a str,
    builder: &'a str,
    arguments: Vec<&'a str>,
    environment: Vec<EnvironmentRequest<'a>>,
    build_mode: u32,
}

#[derive(serde::Serialize)]
struct OutputRequest<'a> {
    name: &'a str,
    path: &'a str,
}

#[derive(serde::Serialize)]
struct EnvironmentRequest<'a> {
    key: &'a str,
    value: &'a str,
}

impl BuildBackend for NixStoreExecutor {
    fn execute_with_logs(
        &mut self,
        request: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        self.execute_request(request, logs, cancelled)
    }
}

impl NixStoreExecutor {
    pub fn execute(&mut self, request: &BuildExecution<'_>) -> io::Result<BuildResult> {
        BuildBackend::execute(self, request)
    }

    pub fn execute_with_logs(
        &mut self,
        request: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<BuildResult> {
        BuildBackend::execute_with_logs(self, request, logs, &mut || Ok(false))
    }

    pub fn execute_with_cancellation(
        &mut self,
        request: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        BuildBackend::execute_with_logs(self, request, logs, cancelled)
    }
}

fn encode_request(request: &BuildExecution<'_>) -> io::Result<Vec<u8>> {
    fn text(bytes: &[u8]) -> io::Result<&str> {
        std::str::from_utf8(bytes).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "BuildRequest contains invalid UTF-8",
            )
        })
    }

    let build = request.build();
    let outputs = build
        .expected_outputs()
        .iter()
        .map(|(name, path)| {
            Ok(OutputRequest {
                name: text(name)?,
                path: text(path)?,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let input_sources = build
        .input_sources()
        .iter()
        .map(|value| text(value))
        .collect::<io::Result<Vec<_>>>()?;
    let arguments = build
        .arguments()
        .iter()
        .map(|value| text(value))
        .collect::<io::Result<Vec<_>>>()?;
    let environment = build
        .environment()
        .iter()
        .map(|(key, value)| {
            Ok(EnvironmentRequest {
                key: text(key)?,
                value: text(value)?,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    serde_json::to_vec(&ExecutionRequest {
        version: 1,
        request_id: request.request_id(),
        derivation_path: text(build.derivation_path())?,
        outputs,
        input_sources,
        system: build.system(),
        builder: text(build.builder())?,
        arguments,
        environment,
        build_mode: 0,
    })
    .map_err(io::Error::other)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionResponse {
    version: u32,
    success: bool,
    status: String,
    outputs: Vec<(String, String)>,
}

fn parse_response(bytes: &[u8], build: &BuildRequest) -> io::Result<BuildResult> {
    let response: ExecutionResponse = serde_json::from_slice(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid build helper response"))?;
    if response.version != 1 || !response.success {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid build helper response",
        ));
    }
    let status = match response.status.as_str() {
        "built" => BuildStatus::Built,
        "already-valid" => BuildStatus::AlreadyValid,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid build helper status",
            ));
        }
    };
    if response.outputs.len() != build.expected_outputs().len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "build helper output set mismatch",
        ));
    }
    let outputs = response
        .outputs
        .into_iter()
        .map(|(name, path)| (name.into_bytes(), path.into_bytes()))
        .collect::<Vec<_>>();
    if outputs != build.expected_outputs() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "build helper output set mismatch",
        ));
    }
    BuildResult::new(status, outputs, OutputTrust::TrustedExecutor)
}

struct ChildGuard {
    child: Option<std::process::Child>,
}

impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self { child: Some(child) }
    }

    fn kill_and_reap(&mut self) {
        if let Some(mut child) = self.child.take() {
            let pid = rustix::process::Pid::from_raw(child.id() as rustix::process::RawPid);
            if let Some(pid) = pid {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;

    fn deref(&self) -> &Self::Target {
        self.child.as_ref().expect("build helper available")
    }
}

impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.child.as_mut().expect("build helper available")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

fn drain_bounded(mut source: impl Read) -> io::Result<(Vec<u8>, bool)> {
    let mut retained = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0_u8; MAXIMUM_BUILD_LOG_CHUNK_BYTES];
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

fn spawn_reader(
    source: impl Read + Send + 'static,
    error_sender: std::sync::mpsc::Sender<io::Error>,
) -> std::thread::JoinHandle<io::Result<(Vec<u8>, bool)>> {
    std::thread::spawn(move || {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drain_bounded(source))) {
            Ok(result) => {
                if let Err(error) = &result {
                    let _ = error_sender.send(io::Error::new(error.kind(), error.to_string()));
                }
                result
            }
            Err(_) => {
                let error = io::Error::other("build helper output reader panicked");
                let _ = error_sender.send(io::Error::other(error.to_string()));
                Err(error)
            }
        }
    })
}

fn join_reader(
    reader: std::thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
    stream: &str,
) -> io::Result<(Vec<u8>, bool)> {
    reader
        .join()
        .map_err(|_| io::Error::other(format!("build helper {stream} reader panicked")))?
}

fn spawn_log_reader(
    mut source: impl Read + Send + 'static,
    sender: std::sync::mpsc::SyncSender<Vec<u8>>,
    error_sender: std::sync::mpsc::Sender<io::Error>,
) -> std::thread::JoinHandle<io::Result<()>> {
    std::thread::spawn(move || {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
                let mut buffer = [0_u8; MAXIMUM_BUILD_LOG_CHUNK_BYTES];
                loop {
                    let read = source.read(&mut buffer)?;
                    if read == 0 {
                        return Ok(());
                    }
                    if sender.send(buffer[..read].to_vec()).is_err() {
                        return Ok(());
                    }
                }
            }));
        match result {
            Ok(result) => {
                if let Err(error) = &result {
                    let _ = error_sender.send(io::Error::new(error.kind(), error.to_string()));
                }
                result
            }
            Err(_) => {
                let error = io::Error::other("build helper stderr reader panicked");
                let _ = error_sender.send(io::Error::other(error.to_string()));
                Err(error)
            }
        }
    })
}

fn forward_logs(
    receiver: &std::sync::mpsc::Receiver<Vec<u8>>,
    logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<()> {
    while let Ok(chunk) = receiver.try_recv() {
        logs(&chunk)?;
    }
    Ok(())
}

fn join_log_reader(reader: std::thread::JoinHandle<io::Result<()>>) -> io::Result<()> {
    reader
        .join()
        .map_err(|_| io::Error::other("build helper stderr reader panicked"))?
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn configure_child_lifecycle(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
                .map_err(io::Error::from)?;
            rustix::process::setpgid(None, None).map_err(io::Error::from)?;
            if rustix::process::getppid() == Some(rustix::process::Pid::INIT) {
                return Err(io::Error::other("build owner exited before helper start"));
            }
            Ok(())
        });
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn configure_child_lifecycle(_command: &mut std::process::Command) {}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{
        MAXIMUM_BUILD_LOG_CHUNK_BYTES, MAXIMUM_QUEUED_BUILD_LOG_CHUNKS,
        MAXIMUM_QUEUED_BUILD_LOG_PAYLOAD_BYTES, spawn_log_reader,
    };

    struct FailingLogReader;

    impl io::Read for FailingLogReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("log read failed"))
        }
    }

    struct PanickingLogReader;

    impl io::Read for PanickingLogReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("log read panicked")
        }
    }

    struct ChunkSource {
        remaining_chunks: u8,
        started: std::sync::mpsc::Sender<u8>,
    }

    impl io::Read for ChunkSource {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.remaining_chunks == 0 {
                return Ok(0);
            }
            let chunk = self.remaining_chunks;
            self.remaining_chunks -= 1;
            buffer.fill(chunk);
            self.started
                .send(chunk)
                .expect("test receives source progress");
            Ok(buffer.len())
        }
    }

    #[test]
    fn build_log_channel_has_fixed_payload_bound() {
        assert_eq!(MAXIMUM_BUILD_LOG_CHUNK_BYTES, 8192);
        assert_eq!(MAXIMUM_QUEUED_BUILD_LOG_CHUNKS, 8);
        assert_eq!(
            MAXIMUM_QUEUED_BUILD_LOG_PAYLOAD_BYTES,
            MAXIMUM_BUILD_LOG_CHUNK_BYTES * MAXIMUM_QUEUED_BUILD_LOG_CHUNKS
        );
    }

    #[test]
    fn build_log_reader_stalls_when_the_fixed_queue_is_full_and_resumes_after_drain() {
        let (log_sender, log_receiver) = std::sync::mpsc::sync_channel(8);
        let (error_sender, _error_receiver) = std::sync::mpsc::channel();
        let (progress_sender, progress_receiver) = std::sync::mpsc::channel();
        let reader = spawn_log_reader(
            ChunkSource {
                remaining_chunks: 9,
                started: progress_sender,
            },
            log_sender,
            error_sender,
        );

        for expected in (1..=9).rev() {
            assert_eq!(
                progress_receiver.recv().expect("source reaches next chunk"),
                expected
            );
        }
        assert!(
            !reader.is_finished(),
            "reader must block while the ninth chunk waits for queue capacity"
        );

        for expected in (2..=9).rev() {
            assert_eq!(
                log_receiver.recv().expect("queued chunk is received"),
                vec![expected; MAXIMUM_BUILD_LOG_CHUNK_BYTES]
            );
        }
        assert_eq!(
            log_receiver
                .recv()
                .expect("blocked chunk resumes after drain"),
            vec![1; MAXIMUM_BUILD_LOG_CHUNK_BYTES]
        );
        reader
            .join()
            .expect("reader thread does not panic")
            .expect("reader completes after queue drain");
    }

    #[test]
    fn build_log_reader_reports_io_failure_to_execution_owner() {
        let (log_sender, _log_receiver) = std::sync::mpsc::sync_channel(1);
        let (error_sender, error_receiver) = std::sync::mpsc::channel();

        let reader = spawn_log_reader(FailingLogReader, log_sender, error_sender);

        assert_eq!(
            error_receiver
                .recv()
                .expect("reader failure is reported")
                .to_string(),
            "log read failed"
        );
        assert_eq!(
            reader
                .join()
                .expect("reader thread does not panic")
                .expect_err("reader fails")
                .to_string(),
            "log read failed"
        );
    }

    #[test]
    fn build_log_reader_reports_panic_to_execution_owner() {
        let (log_sender, _log_receiver) = std::sync::mpsc::sync_channel(1);
        let (error_sender, error_receiver) = std::sync::mpsc::channel();

        let reader = spawn_log_reader(PanickingLogReader, log_sender, error_sender);

        assert_eq!(
            error_receiver
                .recv()
                .expect("reader panic is reported")
                .to_string(),
            "build helper stderr reader panicked"
        );
        assert_eq!(
            reader
                .join()
                .expect("reader panic is contained")
                .expect_err("reader fails")
                .to_string(),
            "build helper stderr reader panicked"
        );
    }
}
