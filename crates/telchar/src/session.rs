use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nix_worker_protocol::{
    ProtocolSessionLimits, WorkerInput, WorkerOperation, WorkerReader,
    write_query_valid_paths_response,
};

use crate::build_request::BuildRequest;
use crate::deployment::DeploymentConfig;
use crate::local_executor::{BuildExecutor, LocalBuildStatus, LocalExecutionRequest};
use crate::store_query::QueryValidPathsStore;

static BUILD_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static DERIVATION_LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    store_closure: &mut dyn crate::store_closure::StoreClosureBackend,
    store_retention: &mut dyn crate::store_retention::StoreRetentionBackend,
    database_url: &str,
    session_id: &str,
    transfer_limits: &crate::transfer_limits::TransferLimits,
    object_admission: &crate::transfer_limits::ObjectAdmissionState,
    rate_admission: &crate::transfer_limits::RateAdmissionState,
    disk_reserve: crate::disk_reserve::DiskReserve,
    disk_probe: &dyn crate::disk_reserve::DiskReserveProbe,
) -> io::Result<()> {
    let mut inbound_budget =
        crate::transfer_limits::TransferBudget::new(transfer_limits.maximum_inbound_session_bytes);
    let mut outbound_budget =
        crate::transfer_limits::TransferBudget::new(transfer_limits.maximum_outbound_session_bytes);
    let mut object_counts = crate::transfer_limits::ObjectSessionCounts::default();
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
                if let Err(error) = disk_reserve.admit_build(
                    disk_probe,
                    std::path::Path::new(crate::disk_reserve::GATEWAY_STORE_DIRECTORY),
                ) {
                    disk_reserve_rejected("build", disk_reserve, error);
                    return reject(
                        &mut output,
                        "gateway-disk-reserve",
                        disk_reserve_error(error),
                    );
                }
                let derivation_path = match std::str::from_utf8(admitted.derivation_path()) {
                    Ok(path) => path,
                    Err(_) => {
                        return reject(
                            &mut output,
                            "invalid-build-derivation",
                            "invalid BuildDerivation request",
                        );
                    }
                };
                let request_id = build_request_id();
                if let Err(error) = crate::persistence::create_build_request(
                    database_url,
                    &request_id,
                    derivation_path,
                    admitted.system(),
                ) {
                    tracing::warn!(
                        event = "database.build_request.failed",
                        operation = "create",
                        failure_class = error.failure().as_str(),
                        "build request persistence failed"
                    );
                    return reject(
                        &mut output,
                        "build-request-state",
                        "build request state operation failed",
                    );
                }
                tracing::info!(
                    event = "database.build_request.created",
                    operation = "create",
                    "build request persisted"
                );
                let lease_id = derivation_lease_id();
                let derivation_entries = [crate::store_retention::RetentionEntry::new(
                    lease_id.clone(),
                    derivation_path,
                )];
                let retained_derivation = match store_retention.retain(&derivation_entries) {
                    Ok(retained) => {
                        retention_batch_event("retain", "derivation", 1, "succeeded", None);
                        retained
                    }
                    Err(_) => {
                        retention_batch_event("retain", "derivation", 1, "failed", Some("helper"));
                        return reject(
                            &mut output,
                            "gateway-store-retention",
                            "gateway store retention failed",
                        );
                    }
                };
                if let Err(_error) = crate::persistence::create_store_lease(
                    database_url,
                    &lease_id,
                    crate::persistence::StoreLeaseOwnerKind::Request,
                    &request_id,
                    derivation_path,
                    crate::persistence::StoreLeasePurpose::Derivation,
                ) {
                    if store_retention.rollback(&retained_derivation).is_err() {
                        retention_batch_event(
                            "rollback",
                            "derivation",
                            1,
                            "failed",
                            Some("rollback"),
                        );
                        return reject(
                            &mut output,
                            "gateway-store-retention",
                            "gateway store retention failed",
                        );
                    }
                    retention_batch_event("rollback", "derivation", 1, "succeeded", None);
                    return reject(
                        &mut output,
                        "store-lease-state",
                        "store lease state operation failed",
                    );
                }
                let closure = match store_closure.input_closure(admitted.input_sources()) {
                    Ok(closure) => closure,
                    Err(_) => {
                        return reject(
                            &mut output,
                            "input-closure-query",
                            "input closure query failed",
                        );
                    }
                };
                let input_leases = closure
                    .into_iter()
                    .enumerate()
                    .map(|(index, path)| (format!("input-{request_id}-{index}"), path))
                    .collect::<Vec<_>>();
                let input_entries = input_leases
                    .iter()
                    .map(|(lease_id, store_path)| {
                        crate::store_retention::RetentionEntry::new(lease_id, store_path)
                    })
                    .collect::<Vec<_>>();
                let retained_inputs = match store_retention.retain(&input_entries) {
                    Ok(retained) => {
                        retention_batch_event(
                            "retain",
                            "input",
                            input_entries.len(),
                            "succeeded",
                            None,
                        );
                        retained
                    }
                    Err(_) => {
                        retention_batch_event(
                            "retain",
                            "input",
                            input_entries.len(),
                            "failed",
                            Some("helper"),
                        );
                        return reject(
                            &mut output,
                            "gateway-store-retention",
                            "gateway store retention failed",
                        );
                    }
                };
                if crate::persistence::create_request_input_leases(
                    database_url,
                    &request_id,
                    &input_leases,
                )
                .is_err()
                {
                    if store_retention.rollback(&retained_inputs).is_err() {
                        retention_batch_event(
                            "rollback",
                            "input",
                            input_entries.len(),
                            "failed",
                            Some("rollback"),
                        );
                        return reject(
                            &mut output,
                            "gateway-store-retention",
                            "gateway store retention failed",
                        );
                    }
                    retention_batch_event(
                        "rollback",
                        "input",
                        input_entries.len(),
                        "succeeded",
                        None,
                    );
                    return reject(
                        &mut output,
                        "store-lease-state",
                        "store lease state operation failed",
                    );
                }
                if let Err(error) =
                    crate::persistence::attach_request(database_url, session_id, &request_id)
                {
                    tracing::warn!(
                        event = "database.request_attachment.failed",
                        operation = "attach",
                        failure_class = error.failure().as_str(),
                        "request attachment persistence failed"
                    );
                    if release_unattached_request_leases(store_retention, database_url, &request_id)
                        .is_err()
                    {
                        return reject(
                            &mut output,
                            "store-lease-state",
                            "store lease state operation failed",
                        );
                    }
                    return reject(
                        &mut output,
                        "request-attachment-state",
                        "request attachment state operation failed",
                    );
                }
                tracing::info!(
                    event = "database.request_attachment.attached",
                    operation = "attach",
                    state = "attached",
                    "request attachment persisted"
                );
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
                let execution = match LocalExecutionRequest::new(
                    &request_id,
                    &admitted,
                    Duration::from_secs(30 * 60),
                ) {
                    Ok(execution) => execution,
                    Err(error) => {
                        if release_attached_request_leases(
                            store_retention,
                            database_url,
                            session_id,
                            &request_id,
                        )
                        .is_err()
                        {
                            return reject(
                                &mut output,
                                "store-lease-state",
                                "store lease state operation failed",
                            );
                        }
                        return Err(error);
                    }
                };
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
                        if release_attached_request_leases(
                            store_retention,
                            database_url,
                            session_id,
                            &request_id,
                        )
                        .is_err()
                        {
                            return reject(
                                &mut output,
                                "store-lease-state",
                                "store lease state operation failed",
                            );
                        }
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
                if release_attached_request_leases(
                    store_retention,
                    database_url,
                    session_id,
                    &request_id,
                )
                .is_err()
                {
                    return reject(
                        &mut output,
                        "store-lease-state",
                        "store lease state operation failed",
                    );
                }
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
                object_counts.admit_outbound(transfer_limits.maximum_outbound_session_objects)?;
                let _permit = object_admission.admit_outbound()?;
                output.write_all(&nix_worker_protocol::STDERR_LAST.to_le_bytes())?;
                let verified = match crate::store_export::export_verified_nar_with_limits_and_rate(
                    path,
                    &mut output,
                    store_export,
                    transfer_limits,
                    &mut outbound_budget,
                    rate_admission,
                ) {
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
                let mut disk_rejection = None;
                let staging_directory = store_import.staging_directory().map(Path::to_path_buf);
                let request = match reader.complete_add_multiple_to_store(
                    negotiated.version,
                    |info, source| {
                        object_counts
                            .admit_inbound(transfer_limits.maximum_inbound_session_objects)?;
                        let _permit = object_admission.admit_inbound()?;
                        if let Some(staging_directory) = staging_directory.as_deref()
                            && let Err(error) = disk_reserve.admit_transfer(
                                disk_probe,
                                std::path::Path::new(crate::disk_reserve::GATEWAY_STORE_DIRECTORY),
                                staging_directory,
                                info.nar_size(),
                            )
                        {
                            disk_reserve_rejected("transfer", disk_reserve, error);
                            disk_rejection = Some(error);
                            return Err(io::Error::other("disk reserve rejected"));
                        }
                        let mut limited = crate::transfer_limits::LimitedReader::with_rate(
                            source,
                            transfer_limits.maximum_object_bytes,
                            &mut inbound_budget,
                            rate_admission.clone(),
                        );
                        store_import.import(info, &mut limited)
                    },
                ) {
                    Ok(request) => request,
                    Err(error) => {
                        if let Some(error) = disk_rejection {
                            return reject(
                                &mut output,
                                "gateway-disk-reserve",
                                disk_reserve_error(error),
                            );
                        }
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

fn build_request_id() -> String {
    format!(
        "request-{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        BUILD_REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed),
    )
}

fn derivation_lease_id() -> String {
    format!(
        "lease-{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        DERIVATION_LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
    )
}

fn retention_batch_event(
    operation: &'static str,
    purpose: &'static str,
    path_count: usize,
    result: &'static str,
    failure_class: Option<&'static str>,
) {
    tracing::info!(
        event = "gateway.store_retention",
        operation,
        purpose,
        path_count,
        result,
        failure_class,
        "gateway store retention batch completed"
    );
}

fn disk_reserve_error(error: crate::disk_reserve::AdmissionFailure) -> &'static str {
    match error.reason() {
        crate::disk_reserve::RejectionReason::InsufficientSpace => "gateway disk reserve exceeded",
        crate::disk_reserve::RejectionReason::ProbeFailed
        | crate::disk_reserve::RejectionReason::ArithmeticOverflow => {
            "gateway disk reserve check failed"
        }
    }
}

fn disk_reserve_rejected(
    operation: &'static str,
    reserve: crate::disk_reserve::DiskReserve,
    error: crate::disk_reserve::AdmissionFailure,
) {
    let reason = match error.reason() {
        crate::disk_reserve::RejectionReason::InsufficientSpace => "insufficient-space",
        crate::disk_reserve::RejectionReason::ProbeFailed => "probe-failed",
        crate::disk_reserve::RejectionReason::ArithmeticOverflow => "arithmetic-overflow",
    };
    if let Some(available_bytes) = error.available_bytes() {
        tracing::warn!(
            event = "worker.disk_reserve.rejected",
            operation,
            filesystem = error.filesystem(),
            configured_reserve_bytes = reserve.bytes(),
            required_bytes = error.required_bytes(),
            available_bytes,
            reason,
            "gateway disk reserve rejected admission"
        );
    } else {
        tracing::warn!(
            event = "worker.disk_reserve.rejected",
            operation,
            filesystem = error.filesystem(),
            configured_reserve_bytes = reserve.bytes(),
            required_bytes = error.required_bytes(),
            reason,
            "gateway disk reserve rejected admission"
        );
    }
}

fn release_attached_request_leases(
    store_retention: &mut dyn crate::store_retention::StoreRetentionBackend,
    database_url: &str,
    session_id: &str,
    request_id: &str,
) -> io::Result<()> {
    let released =
        crate::persistence::detach_request_and_release_leases(database_url, session_id, request_id)
            .map_err(|error| {
                tracing::warn!(
                    event = "database.request_lease_release.failed",
                    operation = "detach-release",
                    failure_class = error.failure().as_str(),
                    "request lease release failed"
                );
                io::Error::other("store lease state operation failed")
            })?;
    release_committed_request_roots(store_retention, &released.leases)
}

fn release_unattached_request_leases(
    store_retention: &mut dyn crate::store_retention::StoreRetentionBackend,
    database_url: &str,
    request_id: &str,
) -> io::Result<()> {
    let released = crate::persistence::release_unattached_request_leases(database_url, request_id)
        .map_err(|error| {
            tracing::warn!(
                event = "database.request_lease_release.failed",
                operation = "unattached-release",
                failure_class = error.failure().as_str(),
                "request lease release failed"
            );
            io::Error::other("store lease state operation failed")
        })?;
    release_committed_request_roots(store_retention, &released.leases)
}

fn release_committed_request_roots(
    store_retention: &mut dyn crate::store_retention::StoreRetentionBackend,
    leases: &[crate::persistence::StoreLeaseRecord],
) -> io::Result<()> {
    let entries = leases
        .iter()
        .map(|lease| {
            crate::store_retention::ReleasedRetentionEntry::new(
                lease.lease_id.clone(),
                lease.store_path.clone(),
            )
        })
        .collect::<Vec<_>>();
    store_retention.release(&entries).map_err(|_| {
        tracing::warn!(
            event = "gateway.request_lease_release.failed",
            operation = "detach-release",
            failure_class = "retention",
            "request root release failed"
        );
        io::Error::other("gateway store retention failed")
    })
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
