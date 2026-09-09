//! Executes and recovers builds through operator-configured SSH builders with bounded streaming and exact output import.

use std::io::{self, Read, Seek, Write};
use std::os::unix::process::CommandExt;
use std::process::{ChildStdin, ChildStdout, Stdio};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use nix_worker_protocol::{
    AddToStoreNarInfo, BuildDerivationClientRequest, BuildDerivationOutputRequest,
    WorkerBuildStatus, WorkerClient, WorkerPathInfo,
};

use crate::backend::{BuildBackend, BuildExecution, BuildResult, BuildStatus, OutputTrust};
use crate::service::config::StaticSshBackendConfig;
use crate::store::closure::{GatewayStoreClosureBackend, StoreClosureBackend};
use crate::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

#[path = "static_ssh/health.rs"]
mod health;

pub use health::{StaticSshHealth, StaticSshHealthCounts, StaticSshHealthState};

const MAXIMUM_BUILD_LOG_CHUNK_BYTES: usize = 8192;
const MAXIMUM_QUEUED_BUILD_LOG_CHUNKS: usize = 8;

pub fn recover_outputs(
    config: &StaticSshBackendConfig,
    gateway: &GatewayStoreEndpoint,
    outputs: &[String],
    timeout: Duration,
) -> io::Result<()> {
    let mut command = ssh_command(config);
    let mut child = ChildGuard::new(command.spawn()?);
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("static SSH stdin is unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("static SSH stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("static SSH stderr is unavailable"))?;
    let stderr_reader = std::thread::spawn(move || {
        let mut reader = stderr.take(MAXIMUM_BUILD_LOG_CHUNK_BYTES as u64);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map(|_| ())
    });
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let gateway = gateway.clone();
    let outputs = outputs.to_vec();
    let worker = std::thread::spawn(move || {
        let result = recover_remote_outputs(WorkerStream { stdin, stdout }, &gateway, &outputs);
        let _ = sender.send(result);
    });
    let result = match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            child.kill_and_reap();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "static SSH output recovery timed out",
            ))
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("static SSH output recovery failed"))
        }
    };
    child.kill_and_reap();
    worker
        .join()
        .map_err(|_| io::Error::other("static SSH output recovery failed"))?;
    stderr_reader
        .join()
        .map_err(|_| io::Error::other("static SSH output recovery failed"))??;
    result
}

fn recover_remote_outputs(
    stream: WorkerStream,
    gateway: &GatewayStoreEndpoint,
    outputs: &[String],
) -> io::Result<()> {
    let mut remote = WorkerClient::connect(stream)?;
    for output in outputs {
        let path = output.as_bytes();
        let info = remote.query_path_info(path)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "remote output is unavailable")
        })?;
        copy_remote_path_to_gateway(&mut remote, gateway, path, &info)?;
        let mut verification = GatewayStoreConnection::connect(gateway)?;
        if verification.query_path_info(path)?.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "build output verification failed",
            ));
        }
    }
    Ok(())
}

pub(super) fn verify_backend(config: &StaticSshBackendConfig, timeout: Duration) -> io::Result<()> {
    let mut command = ssh_command(config);
    let mut child = ChildGuard::new(command.spawn()?);
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("static SSH stdin is unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("static SSH stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("static SSH stderr is unavailable"))?;
    let stderr_reader = std::thread::spawn(move || {
        let mut reader = stderr.take(MAXIMUM_BUILD_LOG_CHUNK_BYTES as u64);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map(|_| ())
    });
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = WorkerClient::connect(WorkerStream { stdin, stdout }).map(|_| ());
        let _ = sender.send(result);
    });
    let result = match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            child.kill_and_reap();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "static SSH verification timed out",
            ))
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("static SSH verification failed"))
        }
    };
    child.kill_and_reap();
    worker
        .join()
        .map_err(|_| io::Error::other("static SSH verification failed"))?;
    stderr_reader
        .join()
        .map_err(|_| io::Error::other("static SSH verification failed"))??;
    result.map_err(|_| io::Error::other("static SSH worker protocol failed"))
}

pub struct StaticSshBackend {
    config: StaticSshBackendConfig,
    gateway: GatewayStoreEndpoint,
}

impl StaticSshBackend {
    pub fn new(config: StaticSshBackendConfig, gateway: GatewayStoreEndpoint) -> Self {
        Self { config, gateway }
    }

    fn execute_request(
        &mut self,
        execution: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        let started = Instant::now();
        tracing::trace!(
            event = "backend.static_ssh.transport.started",
            operation = "execute",
            backend_name = self.config.target().name(),
            "static SSH transport operation started"
        );
        let mut command = ssh_command(&self.config);
        let mut child = ChildGuard::new(command.spawn()?);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("static SSH stdin is unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("static SSH stdout is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("static SSH stderr is unavailable"))?;
        let (log_sender, log_receiver) =
            std::sync::mpsc::sync_channel(MAXIMUM_QUEUED_BUILD_LOG_CHUNKS);
        let (error_sender, error_receiver) = std::sync::mpsc::channel();
        let stderr_reader = spawn_log_reader(stderr, log_sender.clone(), error_sender);
        let (result_sender, result_receiver) = std::sync::mpsc::sync_channel(1);
        let gateway = self.gateway.clone();
        let build = execution.build();
        let deadline = Instant::now() + execution.timeout();

        let result = std::thread::scope(|scope| {
            let worker = scope.spawn(move || {
                let result = execute_remote_build(
                    WorkerStream { stdin, stdout },
                    &gateway,
                    build,
                    &log_sender,
                );
                let _ = result_sender.send(result);
            });
            loop {
                forward_logs(&log_receiver, logs)?;
                if let Ok(error) = error_receiver.try_recv() {
                    child.kill_and_reap();
                    return Err(error);
                }
                if let Ok(result) = result_receiver.try_recv() {
                    worker
                        .join()
                        .map_err(|_| io::Error::other("static SSH worker failed"))?;
                    forward_logs(&log_receiver, logs)?;
                    break result;
                }
                if cancelled()? {
                    child.kill_and_reap();
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "build requester disconnected",
                    ));
                }
                if Instant::now() >= deadline {
                    child.kill_and_reap();
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "build timed out"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })?;
        child.kill_and_reap();
        drop(log_receiver);
        join_log_reader(stderr_reader)?;
        tracing::trace!(
            event = "backend.static_ssh.transport.completed",
            operation = "execute",
            backend_name = self.config.target().name(),
            result = "succeeded",
            duration_ms = started.elapsed().as_millis(),
            "static SSH transport operation completed"
        );
        Ok(result)
    }
}

impl BuildBackend for StaticSshBackend {
    fn execute_with_logs(
        &mut self,
        execution: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        self.execute_request(execution, logs, cancelled)
    }
}

fn execute_remote_build(
    stream: WorkerStream,
    gateway: &GatewayStoreEndpoint,
    build: &crate::build::BuildRequest,
    logs: &std::sync::mpsc::SyncSender<Vec<u8>>,
) -> io::Result<BuildResult> {
    let connected_started = Instant::now();
    let mut remote = WorkerClient::connect(stream)
        .map_err(|_| io::Error::other("static SSH worker protocol failed"))?;
    tracing::trace!(
        event = "backend.static_ssh.protocol.connected",
        duration_ms = connected_started.elapsed().as_millis(),
        "static SSH worker protocol connected"
    );
    let roots = build.input_sources().to_vec();
    let mut closure = GatewayStoreClosureBackend::new(gateway.clone());
    let closure_started = Instant::now();
    tracing::trace!(
        event = "backend.static_ssh.closure.started",
        root_count = roots.len(),
        "static SSH closure query started"
    );
    let closure = closure.input_closure(&roots)?;
    tracing::trace!(
        event = "backend.static_ssh.closure.completed",
        path_count = closure.len(),
        duration_ms = closure_started.elapsed().as_millis(),
        "static SSH closure query completed"
    );
    let closure_paths = closure
        .iter()
        .map(|path| path.store_path.as_bytes().to_vec())
        .collect::<Vec<_>>();
    let missing_started = Instant::now();
    let missing = remote.query_missing(&closure_paths)?;
    let inputs = select_missing_inputs(closure, &missing)?;
    tracing::debug!(
        event = "backend.static_ssh.inputs.selected",
        closure_path_count = closure_paths.len(),
        missing_path_count = inputs.len(),
        duration_ms = missing_started.elapsed().as_millis(),
        "static SSH missing inputs selected"
    );
    let input_count = inputs.len();
    let mut source = GatewayStoreConnection::connect(gateway)?;
    for path in inputs {
        copy_gateway_path_to_remote(&mut source, &mut remote, &path)?;
    }

    tracing::debug!(
        event = "backend.static_ssh.inputs.staged",
        path_count = input_count,
        "static SSH inputs staged"
    );
    let outputs = build
        .output_authorities()
        .iter()
        .map(|output| BuildDerivationOutputRequest {
            name: output.name(),
            path: output.path(),
            hash_algorithm: output.hash_algorithm(),
            hash: output.hash(),
        })
        .collect::<Vec<_>>();
    let request = BuildDerivationClientRequest {
        drv_path: build.derivation_path(),
        outputs: &outputs,
        input_sources: build.input_sources(),
        platform: build.system().as_bytes(),
        builder: build.builder(),
        arguments: build.arguments(),
        environment: build.environment(),
    };
    let build_started = Instant::now();
    tracing::trace!(
        event = "backend.static_ssh.protocol.started",
        operation = "build_derivation",
        output_count = outputs.len(),
        "static SSH worker protocol operation started"
    );
    let remote_result =
        remote.build_derivation(&request, &mut |message| send_logs(logs, message))?;
    tracing::trace!(
        event = "backend.static_ssh.protocol.completed",
        operation = "build_derivation",
        result = "succeeded",
        duration_ms = build_started.elapsed().as_millis(),
        "static SSH worker protocol operation completed"
    );
    let mut actual_outputs = remote_result.outputs().to_vec();
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
        let info = remote.query_path_info(path)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "remote output is unavailable")
        })?;
        copy_remote_path_to_gateway(&mut remote, gateway, path, &info)?;
        let mut verification = GatewayStoreConnection::connect(gateway)?;
        if verification.query_path_info(path)?.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "build output verification failed",
            ));
        }
    }

    BuildResult::new(
        match remote_result.status() {
            WorkerBuildStatus::Built => BuildStatus::Built,
            WorkerBuildStatus::AlreadyValid => BuildStatus::AlreadyValid,
        },
        build.expected_outputs().to_vec(),
        OutputTrust::TrustedExecutor,
    )
}

fn select_missing_inputs(
    closure: Vec<crate::store::closure::ClosurePath>,
    missing: &nix_worker_protocol::WorkerMissingPaths,
) -> io::Result<Vec<crate::store::closure::ClosurePath>> {
    let missing_paths = missing
        .will_build
        .iter()
        .chain(&missing.will_substitute)
        .chain(&missing.unknown)
        .map(Vec::as_slice)
        .collect::<std::collections::BTreeSet<_>>();
    if missing_paths.len()
        != missing.will_build.len() + missing.will_substitute.len() + missing.unknown.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote missing input set contains duplicates",
        ));
    }
    let selected = closure
        .into_iter()
        .filter(|path| missing_paths.contains(path.store_path.as_bytes()))
        .collect::<Vec<_>>();
    if selected.len() != missing_paths.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote missing input set contains an unexpected path",
        ));
    }
    Ok(selected)
}

fn copy_gateway_path_to_remote(
    gateway: &mut GatewayStoreConnection,
    remote: &mut WorkerClient<WorkerStream>,
    path: &crate::store::closure::ClosurePath,
) -> io::Result<()> {
    let started = Instant::now();
    crate::service::metrics::transfer_started("outbound", "build_input", "static_ssh");
    let references = path
        .references
        .iter()
        .map(|value| value.as_bytes().to_vec())
        .collect::<Vec<_>>();
    let metadata = AddToStoreNarInfo {
        path: path.store_path.as_bytes(),
        deriver: path.deriver.as_ref().map(|value| value.as_bytes()),
        nar_hash_hex: &path.nar_hash,
        references: &references,
        registration_time: 0,
        nar_size: path.nar_size,
        ultimate: false,
        signatures: &[],
        content_address: path.content_address.as_ref().map(|value| value.as_bytes()),
    };
    let result = (|| {
        let mut nar = create_staging_file()?;
        gateway.nar_from_path(path.store_path.as_bytes(), path.nar_size, &mut nar)?;
        nar.rewind()?;
        remote.add_to_store_nar(&metadata, &mut nar, false, true)
    })();
    match &result {
        Ok(()) => crate::service::metrics::transfer_finished(
            "outbound",
            "build_input",
            "static_ssh",
            path.nar_size,
            started.elapsed(),
        ),
        Err(error) => crate::service::metrics::transfer_failed(
            "outbound",
            "build_input",
            "static_ssh",
            crate::service::metrics::io_failure_class(error),
            started.elapsed(),
        ),
    }
    result
}

fn copy_remote_path_to_gateway(
    remote: &mut WorkerClient<WorkerStream>,
    gateway: &GatewayStoreEndpoint,
    path: &[u8],
    metadata: &WorkerPathInfo,
) -> io::Result<()> {
    let started = Instant::now();
    crate::service::metrics::transfer_started("inbound", "build_output", "static_ssh");
    let result = (|| {
        let mut nar = create_staging_file()?;
        remote.nar_from_path(path, metadata.nar_size(), &mut nar)?;
        nar.rewind()?;
        let mut destination = GatewayStoreConnection::connect(gateway)?;
        destination
            .add_to_store_nar(&add_info(path, metadata), &mut nar, false, true)
            .map_err(|error| io::Error::new(error.kind(), "static SSH output import failed"))
    })();
    record_static_ssh_transfer(result, "inbound", "build_output", metadata, started)
}

fn record_static_ssh_transfer(
    result: io::Result<()>,
    direction: &str,
    purpose: &str,
    metadata: &WorkerPathInfo,
    started: Instant,
) -> io::Result<()> {
    match &result {
        Ok(()) => crate::service::metrics::transfer_finished(
            direction,
            purpose,
            "static_ssh",
            metadata.nar_size(),
            started.elapsed(),
        ),
        Err(error) => crate::service::metrics::transfer_failed(
            direction,
            purpose,
            "static_ssh",
            crate::service::metrics::io_failure_class(error),
            started.elapsed(),
        ),
    }
    result
}

fn create_staging_file() -> io::Result<std::fs::File> {
    let directory = std::env::var_os("TELCHAR_IMPORT_STAGING_DIRECTORY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("telchar-import"));
    std::fs::create_dir_all(&directory)?;
    for attempt in 0..100 {
        let path = directory.join(format!(
            "static-ssh-output-{}-{attempt}",
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                std::fs::remove_file(path)?;
                return Ok(file);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "static SSH output staging file is unavailable",
    ))
}

fn add_info<'a>(path: &'a [u8], metadata: &'a WorkerPathInfo) -> AddToStoreNarInfo<'a> {
    AddToStoreNarInfo {
        path,
        deriver: metadata.deriver(),
        nar_hash_hex: metadata.nar_hash_hex(),
        references: metadata.references(),
        registration_time: metadata.registration_time(),
        nar_size: metadata.nar_size(),
        ultimate: false,
        signatures: metadata.signatures(),
        content_address: metadata.content_address(),
    }
}

fn send_logs(sender: &std::sync::mpsc::SyncSender<Vec<u8>>, message: &[u8]) -> io::Result<()> {
    for chunk in message.chunks(MAXIMUM_BUILD_LOG_CHUNK_BYTES) {
        sender
            .send(chunk.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "build log receiver closed"))?;
    }
    Ok(())
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

struct WorkerStream {
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl Read for WorkerStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buffer)
    }
}

impl Write for WorkerStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stdin.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
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
        self.child.as_ref().expect("static SSH process available")
    }
}

impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.child.as_mut().expect("static SSH process available")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

fn spawn_log_reader(
    mut source: impl Read + Send + 'static,
    sender: std::sync::mpsc::SyncSender<Vec<u8>>,
    error_sender: std::sync::mpsc::Sender<io::Error>,
) -> std::thread::JoinHandle<io::Result<()>> {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; MAXIMUM_BUILD_LOG_CHUNK_BYTES];
        loop {
            match source.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(_) => {
                    if sender
                        .send(b"static SSH transport diagnostic\n".to_vec())
                        .is_err()
                    {
                        return Ok(());
                    }
                }
                Err(error) => {
                    let _ = error_sender.send(io::Error::new(error.kind(), error.to_string()));
                    return Err(error);
                }
            }
        }
    })
}

fn join_log_reader(reader: std::thread::JoinHandle<io::Result<()>>) -> io::Result<()> {
    reader
        .join()
        .map_err(|_| io::Error::other("static SSH stderr reader panicked"))?
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn ssh_command(config: &StaticSshBackendConfig) -> std::process::Command {
    let mut command = std::process::Command::new(config.ssh_program());
    configure_child_lifecycle(&mut command);
    command
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
        ])
        .arg(format!(
            "UserKnownHostsFile={}",
            config.known_hosts_file().display()
        ))
        .arg("-i")
        .arg(config.identity_file())
        .arg("-p")
        .arg(config.port().to_string())
        .arg(config.destination())
        .arg("nix-daemon --stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn configure_child_lifecycle(command: &mut std::process::Command) {
    unsafe {
        command.pre_exec(|| {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
                .map_err(io::Error::from)?;
            rustix::process::setpgid(None, None).map_err(io::Error::from)?;
            Ok(())
        });
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn configure_child_lifecycle(_command: &mut std::process::Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use nix_worker_protocol::WorkerMissingPaths;

    fn closure_path(store_path: &str) -> crate::store::closure::ClosurePath {
        crate::store::closure::ClosurePath {
            store_path: store_path.to_owned(),
            nar_hash: "sha256:00".to_owned(),
            nar_size: 1,
            references: Vec::new(),
            deriver: None,
            content_address: None,
        }
    }

    #[test]
    fn forwards_all_queued_logs() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(2);
        sender.send(b"first\n".to_vec()).expect("first log queues");
        sender.send(b"last\n".to_vec()).expect("last log queues");
        let mut forwarded = Vec::new();

        forward_logs(&receiver, &mut |chunk| {
            forwarded.extend_from_slice(chunk);
            Ok(())
        })
        .expect("logs forward");

        assert_eq!(forwarded, b"first\nlast\n");
    }

    #[test]
    fn stages_only_paths_reported_missing_by_remote_store() {
        let present = "/nix/store/00000000000000000000000000000000-present";
        let missing = "/nix/store/11111111111111111111111111111111-missing";
        let closure = vec![closure_path(present), closure_path(missing)];
        let remote = WorkerMissingPaths {
            will_build: Vec::new(),
            will_substitute: Vec::new(),
            unknown: vec![missing.as_bytes().to_vec()],
            download_size: 0,
            nar_size: 0,
        };

        let selected = select_missing_inputs(closure, &remote).expect("missing inputs select");

        assert_eq!(
            selected
                .iter()
                .map(|path| path.store_path.as_str())
                .collect::<Vec<_>>(),
            vec![missing]
        );
    }
}
