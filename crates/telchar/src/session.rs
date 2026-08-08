use std::io::{self, Write};
use std::time::Duration;

use nix_worker_protocol::{
    write_query_valid_paths_response, ProtocolSessionLimits, WorkerInput, WorkerOperation,
    WorkerReader,
};

use crate::build_request::BuildRequest;
use crate::deployment::DeploymentConfig;
use crate::local_executor::{BuildExecutor, LocalBuildStatus, LocalExecutionRequest};
use crate::store_query::QueryValidPathsStore;

pub struct SessionInput {
    input: std::os::unix::net::UnixStream,
    idle_timeout: Duration,
    deadline: Option<std::time::Instant>,
}

impl SessionInput {
    pub fn new(input: std::os::unix::net::UnixStream, idle_timeout: Duration) -> Self {
        Self {
            input,
            idle_timeout,
            deadline: None,
        }
    }
}

fn requester_disconnected(stream: &mut std::os::unix::net::UnixStream) -> io::Result<bool> {
    let mut descriptor = [rustix::event::PollFd::new(
        &*stream,
        rustix::event::PollFlags::IN | rustix::event::PollFlags::HUP,
    )];
    let timeout = rustix::time::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    rustix::event::poll(&mut descriptor, Some(&timeout))?;
    let events = descriptor[0].revents();
    if events.contains(rustix::event::PollFlags::HUP) {
        return Ok(true);
    }
    if events.contains(rustix::event::PollFlags::IN) {
        let mut byte = [std::mem::MaybeUninit::uninit(); 1];
        match rustix::net::recv(
            &*stream,
            &mut byte,
            rustix::net::RecvFlags::PEEK | rustix::net::RecvFlags::DONTWAIT,
        ) {
            Ok((_, 0)) => return Ok(true),
            Ok(_) => {}
            Err(rustix::io::Errno::WOULDBLOCK) => {}
            Err(error) => return Err(io::Error::from(error)),
        }
    }
    Ok(false)
}

impl io::Read for SessionInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let timeout = self
            .deadline
            .map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()));
        self.input.set_read_timeout(timeout)?;
        let received = self.input.read(buffer).map_err(|error| {
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) {
                io::Error::new(io::ErrorKind::TimedOut, "worker protocol input timed out")
            } else {
                error
            }
        })?;
        if received > 0 {
            self.deadline = Some(std::time::Instant::now() + self.idle_timeout);
        }
        Ok(received)
    }
}

impl WorkerInput for SessionInput {
    fn complete_message(&mut self) {
        self.deadline = None;
        let _ = self.input.set_read_timeout(None);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_worker_session(
    input: std::os::unix::net::UnixStream,
    mut output: std::os::unix::net::UnixStream,
    limits: ProtocolSessionLimits,
    deployment: &DeploymentConfig,
    store_query: &mut dyn QueryValidPathsStore,
    build_executor: &mut dyn BuildExecutor,
    store_export: &mut dyn crate::store_export::StoreExportBackend,
    store_import: &mut dyn crate::store_import::StoreImportBackend,
    request_id: &str,
) -> io::Result<()> {
    let mut cancellation_input = input.try_clone()?;
    let input = SessionInput::new(input, limits.incomplete_message_idle_timeout);
    let mut reader = WorkerReader::new(input, limits);
    let negotiated = reader.perform_server_handshake(&mut output, &[])?;
    reader.complete_server_post_handshake(&mut output, negotiated.version, "telchar")?;

    loop {
        match reader.read_operation() {
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                tracing::error!(
                    event = "worker.session.timed_out",
                    "worker protocol session timed out"
                );
                return Ok(());
            }
            Err(_) => {
                return reject(&mut output, "unknown-operation", "unknown worker operation");
            }
            Ok(WorkerOperation::BuildDerivation) => {
                let request = match reader.complete_build_derivation() {
                    Ok(request) => request,
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                        tracing::error!(
                            event = "worker.session.timed_out",
                            "worker protocol session timed out"
                        );
                        return Ok(());
                    }
                    Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                        return reject(
                            &mut output,
                            "invalid-build-derivation",
                            "invalid BuildDerivation request",
                        );
                    }
                    Err(_) => {
                        return reject(
                            &mut output,
                            "invalid-build-derivation",
                            "invalid BuildDerivation request",
                        );
                    }
                };
                let admitted = match BuildRequest::from_worker_request(&request, deployment) {
                    Ok(admitted) => admitted,
                    Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                        return reject(
                            &mut output,
                            "unsupported-build-derivation",
                            "unsupported BuildDerivation request",
                        );
                    }
                    Err(_) => {
                        return reject(
                            &mut output,
                            "invalid-build-derivation",
                            "invalid BuildDerivation request",
                        );
                    }
                };
                tracing::info!(
                    event = "worker.build_derivation.admitted",
                    output_count = admitted.expected_outputs().len(),
                    input_count = admitted.input_sources().len(),
                    argument_count = admitted.arguments().len(),
                    environment_count = admitted.environment().len(),
                    configured_system = deployment.system(),
                    requested_system = request.platform(),
                    build_mode = request.build_mode(),
                    "BuildDerivation request admitted"
                );
                let execution = LocalExecutionRequest::new(
                    request_id,
                    &admitted,
                    Duration::from_secs(30 * 60),
                )?;
                let result = match build_executor.execute_with_logs(
                    &execution,
                    &mut |chunk| {
                        nix_worker_protocol::write_stderr_frame(
                            &mut output,
                            nix_worker_protocol::StderrFrame::Next {
                                message: chunk.to_vec(),
                            },
                        )?;
                        output.flush()
                    },
                    &mut || requester_disconnected(&mut cancellation_input),
                ) {
                    Ok(result) => result,
                    Err(error) => {
                        let unavailable = error.kind() == io::ErrorKind::Unsupported;
                        tracing::error!(
                            event = if unavailable {
                                "worker.build_derivation.execution_unavailable"
                            } else {
                                "worker.build_derivation.failed"
                            },
                            reason = execution_error_reason(&error),
                            "BuildDerivation execution failed"
                        );
                        if error.kind() == io::ErrorKind::ConnectionAborted {
                            return Ok(());
                        }
                        return reject(
                            &mut output,
                            if unavailable {
                                "execution-unavailable"
                            } else {
                                "build-derivation-failed"
                            },
                            if unavailable {
                                "BuildDerivation execution is unavailable"
                            } else {
                                "BuildDerivation execution failed"
                            },
                        );
                    }
                };
                nix_worker_protocol::write_build_derivation_success_response(
                    &mut output,
                    negotiated.version,
                    result.status() == LocalBuildStatus::AlreadyValid,
                )?;
                tracing::info!(
                    event = "worker.build_derivation.completed",
                    output_count = result.outputs().len(),
                    status = ?result.status(),
                    "BuildDerivation execution completed"
                );
            }
            Ok(WorkerOperation::QueryPathInfo) => {
                let request = match reader.complete_store_path_request() {
                    Ok(request) => request,
                    Err(_) => {
                        return reject(
                            &mut output,
                            "invalid-query-path-info",
                            "invalid QueryPathInfo request",
                        );
                    }
                };
                let path = match std::str::from_utf8(request.path()) {
                    Ok(path) => std::path::Path::new(path),
                    Err(_) => {
                        return reject(
                            &mut output,
                            "invalid-query-path-info",
                            "invalid QueryPathInfo request",
                        );
                    }
                };
                let info = match crate::store_export::query_path_info(path, store_export) {
                    Ok(info) => info,
                    Err(error) => {
                        tracing::error!(
                            event = "worker.query_path_info.failed",
                            reason = execution_error_reason(&error),
                            diagnostic = %error,
                            "gateway store QueryPathInfo failed"
                        );
                        return reject(
                            &mut output,
                            "query-path-info-store-failure",
                            "QueryPathInfo store query failed",
                        );
                    }
                };
                let valid = info.is_some();
                output.write_all(&nix_worker_protocol::STDERR_LAST.to_le_bytes())?;
                if let Some(info) = info {
                    let references = info
                        .references
                        .iter()
                        .map(|path| path.as_os_str().as_encoded_bytes().to_vec())
                        .collect::<Vec<_>>();
                    let deriver = info
                        .deriver
                        .as_ref()
                        .map(|path| path.as_os_str().as_encoded_bytes());
                    let nar_hash_hex = info
                        .nar_hash
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    nix_worker_protocol::write_query_path_info_response(
                        &mut output,
                        negotiated.version,
                        Some(nix_worker_protocol::PathInfoResponse {
                            deriver,
                            nar_hash_hex: &nar_hash_hex,
                            references: &references,
                            registration_time: 0,
                            nar_size: info.nar_size,
                            ultimate: false,
                            signatures: &[],
                            content_address: info.content_address.as_deref(),
                        }),
                    )?;
                } else {
                    nix_worker_protocol::write_query_path_info_response(
                        &mut output,
                        negotiated.version,
                        None,
                    )?;
                }
                output.flush()?;
                tracing::info!(
                    event = "worker.query_path_info.completed",
                    valid,
                    "QueryPathInfo request completed"
                );
            }
            Ok(WorkerOperation::NarFromPath) => {
                let request = match reader.complete_store_path_request() {
                    Ok(request) => request,
                    Err(_) => {
                        return reject(
                            &mut output,
                            "invalid-nar-from-path",
                            "invalid NarFromPath request",
                        );
                    }
                };
                let path = match std::str::from_utf8(request.path()) {
                    Ok(path) => std::path::Path::new(path),
                    Err(_) => {
                        return reject(
                            &mut output,
                            "invalid-nar-from-path",
                            "invalid NarFromPath request",
                        );
                    }
                };
                output.write_all(&nix_worker_protocol::STDERR_LAST.to_le_bytes())?;
                let verified =
                    match crate::store_export::export_verified_nar(path, &mut output, store_export)
                    {
                        Ok(verified) => verified,
                        Err(error) => {
                            tracing::error!(
                                event = "worker.nar_from_path.failed",
                                reason = execution_error_reason(&error),
                                "gateway store NarFromPath failed"
                            );
                            return Err(error);
                        }
                    };
                output.flush()?;
                tracing::info!(
                    event = "worker.nar_from_path.completed",
                    nar_size = verified.nar_size,
                    "NarFromPath request completed"
                );
            }
            Ok(WorkerOperation::AddMultipleToStore) => {
                let request = match reader
                    .complete_add_multiple_to_store(negotiated.version, |info, source| {
                        store_import.import(info, source)
                    }) {
                    Ok(request) => request,
                    Err(error) => {
                        tracing::error!(
                            event = "worker.operation.rejected",
                            rejection = "invalid-add-multiple-to-store",
                            reason = error.to_string(),
                            "AddMultipleToStore request rejected"
                        );
                        return reject(
                            &mut output,
                            "invalid-add-multiple-to-store",
                            "invalid AddMultipleToStore request",
                        );
                    }
                };
                output.write_all(&nix_worker_protocol::STDERR_LAST.to_le_bytes())?;
                output.flush()?;
                tracing::info!(
                    event = "worker.add_multiple_to_store.completed",
                    object_count = request.object_count(),
                    repair = request.repair(),
                    dont_check_signatures = request.dont_check_signatures(),
                    "AddMultipleToStore request completed"
                );
            }
            Ok(WorkerOperation::QueryValidPaths) => {
                let request = match reader.complete_query_valid_paths(negotiated.version) {
                    Ok(request) => request,
                    Err(error) => {
                        tracing::error!(
                            event = "worker.operation.rejected",
                            rejection = "invalid-query-valid-paths",
                            reason = error.to_string(),
                            "QueryValidPaths request rejected"
                        );
                        return reject(
                            &mut output,
                            "invalid-query-valid-paths",
                            "invalid QueryValidPaths request",
                        );
                    }
                };
                let requested_count = request.paths().len();
                let valid_paths = match store_query.query_valid_paths(request.paths()) {
                    Ok(paths) => paths,
                    Err(error) => {
                        tracing::error!(
                            event = "worker.query_valid_paths.failed",
                            reason = error.to_string(),
                            "gateway store QueryValidPaths failed"
                        );
                        return reject(
                            &mut output,
                            "query-valid-paths-store-failure",
                            "QueryValidPaths store query failed",
                        );
                    }
                };
                output.write_all(&nix_worker_protocol::STDERR_LAST.to_le_bytes())?;
                write_query_valid_paths_response(&mut output, &valid_paths)?;
                output.flush()?;
                tracing::info!(
                    event = "worker.query_valid_paths.completed",
                    requested_count,
                    valid_count = valid_paths.len(),
                    "QueryValidPaths request completed"
                );
            }
            Ok(WorkerOperation::SetOptions) => {
                match reader.complete_set_options() {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                        tracing::error!(
                            event = "worker.session.timed_out",
                            "worker protocol session timed out"
                        );
                        return Ok(());
                    }
                    Err(_) => {
                        return reject(
                            &mut output,
                            "invalid-set-options",
                            "invalid SetOptions request",
                        );
                    }
                }
                output.write_all(&nix_worker_protocol::STDERR_LAST.to_le_bytes())?;
                output.flush()?;
                tracing::info!(
                    event = "worker.set_options.completed",
                    "SetOptions request completed"
                );
            }
            Ok(operation) if !operation.is_fixture_allowed() => {
                tracing::error!(
                    event = "worker.operation.unsupported",
                    operation = ?operation,
                    "recognized worker operation is unsupported"
                );
                return reject(
                    &mut output,
                    "recognized-unsupported",
                    "unsupported worker operation",
                );
            }
            Ok(operation) => {
                tracing::error!(
                    event = "worker.operation.unimplemented",
                    operation = ?operation,
                    "fixture-observed worker operation is not implemented"
                );
                return reject(
                    &mut output,
                    "recognized-unimplemented",
                    "unsupported worker operation",
                );
            }
        }
    }
}

fn execution_error_reason(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::TimedOut => "timeout",
        io::ErrorKind::InvalidData => "invalid-data",
        io::ErrorKind::InvalidInput => "invalid-input",
        io::ErrorKind::NotFound => "not-found",
        io::ErrorKind::PermissionDenied => "permission-denied",
        _ => "execution-failure",
    }
}

fn reject(output: &mut impl Write, rejection: &str, message: &str) -> io::Result<()> {
    tracing::error!(
        event = "worker.operation.rejected",
        rejection,
        "worker operation rejected"
    );
    nix_worker_protocol::write_worker_error(output, message)?;
    output.flush()
}
