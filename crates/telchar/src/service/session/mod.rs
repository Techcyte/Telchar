//! Runs one authenticated Nix worker session from operation dispatch through durable build completion and cleanup.

use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nix_worker_protocol::{
    ProtocolSessionLimits, WorkerInput, WorkerOperation, WorkerReader,
    write_query_valid_paths_response,
};

use crate::backend::{BuildBackend, BuildExecution, BuildStatus};
use crate::build::BuildRequest;
use crate::store::query::QueryValidPathsStore;

static BUILD_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static DERIVATION_LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static OUTPUT_LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

mod builder;
mod input;

pub use builder::SessionBuilder;
use builder::SessionContext;
use input::{SessionInput, requester_disconnected};

fn run_worker_session(context: SessionContext<'_>) -> io::Result<()> {
    let SessionContext {
        input,
        mut output,
        limits,
        backend_targets,
        running_disconnect_policy,
        output_retention,
        maximum_retained_input_bytes,
        store_query,
        build_executor,
        store_export,
        store_import,
        store_closure,
        store_retention,
        store_substitution,
        database,
        session_id,
        audit_subject,
        quota_subject,
        transfer_limits,
        object_admission,
        rate_admission,
        disk_reserve,
        disk_probe,
        shared_builds,
        shared_build_scheduler,
        scheduling_limits,
        cache_publisher,
    } = context;
    let store_directory = crate::service::disk_reserve::gateway_store_directory()?;
    let mut inbound_budget = crate::service::transfer_limits::TransferBudget::new(
        transfer_limits.maximum_inbound_session_bytes,
    );
    let mut outbound_budget = crate::service::transfer_limits::TransferBudget::new(
        transfer_limits.maximum_outbound_session_bytes,
    );
    let mut object_counts = crate::service::transfer_limits::ObjectSessionCounts::default();
    let mut cancellation_input = input.try_clone()?;
    let input = SessionInput::new(input, limits.incomplete_message_idle_timeout);
    let mut reader = WorkerReader::new(input, limits);
    let handshake = tracing::trace_span!("worker.handshake").entered();
    let negotiated = reader.perform_server_handshake(&mut output, &[])?;
    reader.complete_server_post_handshake(&mut output, negotiated.version, "telchar")?;
    drop(handshake);

    macro_rules! execute_admitted_build {
        ($admitted:expr, $request_started:expr, $build_mode:expr, $requested_system:expr, $write_success:expr $(,)?) => {{
            let admitted = $admitted;
            let request_started = $request_started;
            let build_mode = $build_mode;
            let requested_system = $requested_system;
                if return_cached_outputs(
                    &admitted,
                    database,
                    store_query,
                    store_export,
                    store_retention,
                    output_retention.duration(),
                    audit_subject,
                    quota_subject,
                )? {
                    $write_success(&mut output, negotiated.version, true)?;
                    crate::service::metrics::build_request_finished(
                        request_started.elapsed(), "succeeded", None,
                    );
                    tracing::info!(
                        event = "worker.build_derivation.cached",
                        output_count = admitted.expected_outputs().len(),
                        "cached BuildDerivation outputs returned"
                    );
                    continue;
                }
                if let Err(error) = disk_reserve.admit_build(
                    disk_probe,
                    &store_directory,
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
                    database,
                    &request_id,
                    derivation_path,
                    admitted.system(),
                    audit_subject,
                    quota_subject,
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
                let derivation_info =
                    match store_export.query_path_info(std::path::Path::new(derivation_path)) {
                        Ok(info) if info.nar_size > 0 => info,
                        result => {
                            tracing::error!(
                                event = "gateway.store_retention.failed",
                                operation = "query-derivation-path-info",
                                diagnostic = ?result,
                                "gateway store retention failed"
                            );
                            return reject(
                                &mut output,
                                "gateway-store-retention",
                                "gateway store retention failed",
                            );
                        }
                    };
                let lease_id = derivation_lease_id();
                let derivation_entries = [crate::store::retention::RetentionEntry::new(
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
                if let Err(_error) = crate::persistence::create_request_retained_lease(
                    database,
                    &lease_id,
                    &request_id,
                    derivation_path,
                    crate::persistence::StoreLeasePurpose::Derivation,
                    derivation_info.nar_size,
                    maximum_retained_input_bytes,
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
                    Err(error) => {
                        let (path, path_kind) = crate::store::closure::failure_path(&error)
                            .map(|(path, kind)| (path, kind.as_str()))
                            .unwrap_or(("none", "none"));
                        tracing::error!(
                            event = "gateway.input_closure.failed",
                            phase = crate::store::closure::failure_phase(&error)
                                .map(crate::store::closure::ClosureFailurePhase::as_str)
                                .unwrap_or("unknown"),
                            path,
                            path_kind,
                            "input closure query failed"
                        );
                        if let Err(error) = release_unattached_request_leases(
                            store_retention,
                            database,
                            &request_id,
                        ) {
                            return reject(
                                &mut output,
                                "request-lease-release",
                                release_error_message(&error),
                            );
                        }
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
                    .map(|(index, path)| {
                        (
                            format!("input-{request_id}-{index}"),
                            path.store_path,
                            path.nar_size,
                        )
                    })
                    .collect::<Vec<_>>();
                let input_entries = input_leases
                    .iter()
                    .map(|(lease_id, store_path, _)| {
                        crate::store::retention::RetentionEntry::new(lease_id, store_path)
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
                        if let Err(error) = release_unattached_request_leases(
                            store_retention,
                            database,
                            &request_id,
                        ) {
                            return reject(
                                &mut output,
                                "request-lease-release",
                                release_error_message(&error),
                            );
                        }
                        return reject(
                            &mut output,
                            "gateway-store-retention",
                            "gateway store retention failed",
                        );
                    }
                };
                if crate::persistence::create_request_input_leases_with_limit(
                    database,
                    &request_id,
                    maximum_retained_input_bytes,
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
                    if let Err(error) = release_unattached_request_leases(
                        store_retention,
                        database,
                        &request_id,
                    ) {
                        return reject(
                            &mut output,
                            "request-lease-release",
                            release_error_message(&error),
                        );
                    }
                    return reject(
                        &mut output,
                        "store-lease-state",
                        "store lease state operation failed",
                    );
                }
                if let Err(error) =
                    crate::persistence::attach_request(database, session_id, &request_id)
                {
                    tracing::warn!(
                        event = "database.request_attachment.failed",
                        operation = "attach",
                        failure_class = error.failure().as_str(),
                        "request attachment persistence failed"
                    );
                    if let Err(error) = release_unattached_request_leases(
                        store_retention,
                        database,
                        &request_id,
                    ) {
                        return reject(
                            &mut output,
                            "request-lease-release",
                            release_error_message(&error),
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
                crate::service::metrics::build_admitted(
                    if build_mode == 0 {
                        "normal"
                    } else {
                        "unsupported"
                    },
                    admitted
                        .output_authorities()
                        .iter()
                        .any(|authority| !authority.hash_algorithm().is_empty()),
                    admitted.expected_outputs().len(),
                );
                tracing::info!(
                    event = "worker.build_derivation.admitted",
                    output_count = admitted.expected_outputs().len(),
                    input_count = admitted.input_sources().len(),
                    argument_count = admitted.arguments().len(),
                    environment_count = admitted.environment().len(),
                    requested_system,
                    build_mode,
                    "BuildDerivation request admitted"
                );
                let mut execution =
                    match BuildExecution::new(&request_id, &admitted, Duration::from_secs(30 * 60))
                    {
                        Ok(execution) => execution,
                        Err(error) => {
                            if let Err(release_error) = release_attached_request_leases(
                                store_retention,
                                database,
                                session_id,
                                &request_id,
                            ) {
                                return reject(
                                    &mut output,
                                    "request-lease-release",
                                    release_error_message(&release_error),
                                );
                            }
                            return Err(error);
                        }
                    };
                let requester_detached = std::cell::Cell::new(false);
                let durable_execution_owned = std::cell::Cell::new(false);
                let shared_build_key = admitted.shared_build_key();
                let derivation_path =
                    std::str::from_utf8(admitted.derivation_path()).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "invalid derivation path")
                    })?;
                let expected_outputs = admitted
                    .expected_outputs()
                    .iter()
                    .map(|(_, path)| {
                        std::str::from_utf8(path).map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidData, "invalid output path")
                        })
                    })
                    .collect::<io::Result<Vec<_>>>()?;
                tracing::debug!(event = "server.build.paths", derivation_path, outputs = ?expected_outputs);
                let required_features = admitted
                    .required_system_features()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                let selected_target =
                    build_executor.selected_target(admitted.system(), &required_features)?;
                execution.set_target_name(selected_target.name())?;
                let backend_execution_id =
                    build_executor.execution_id(&selected_target, shared_build_key.as_bytes())?;
                let shared_result = match shared_builds.acquire(&shared_build_key) {
                    crate::shared_build::SharedBuildAccess::Leader(leader) => {
                        crate::service::metrics::shared_build_leader();
                        tracing::info!(
                            event = "shared_build.coalescing.leader",
                            backend_name = selected_target.name(),
                            backend_kind = selected_target.kind().as_str(),
                            "request became shared-build leader"
                        );
                        let execution_started = std::time::Instant::now();
                        let durable_claim = crate::persistence::claim_shared_build_with_request(
                            database,
                            derivation_path,
                            &admitted.shared_build_digest(),
                            selected_target.name(),
                            selected_target.kind(),
                            selected_target.capabilities(),
                            backend_execution_id.as_deref(),
                            &expected_outputs,
                            &admitted,
                        );
                        let durable_claim = match durable_claim {
                            Ok(claim) => claim,
                            Err(error) => {
                                let _ = leader.complete(Err(
                                    crate::shared_build::SharedBuildTerminalFailure::Internal,
                                ));
                                return reject(
                                    &mut output,
                                    "shared-build-state",
                                    &format!("shared build claim failed: {:?}", error.failure()),
                                );
                            }
                        };
                        if durable_claim.ownership
                            == crate::persistence::SharedBuildOwnership::Joined
                        {
                            match durable_claim.build.state {
                                crate::persistence::SharedBuildState::Succeeded => {
                                    crate::service::metrics::shared_build_reused_result();
                                    tracing::info!(
                                        event = "shared_build.result.reused",
                                        backend_name = selected_target.name(),
                                        backend_kind = selected_target.kind().as_str(),
                                        "durable shared-build result reused"
                                    );
                                    match durable_shared_build_result(&durable_claim.build) {
                                        Ok(result) => leader.complete(Ok(result)).map_err(|_| {
                                            io::Error::other(
                                                "shared BuildDerivation execution failed",
                                            )
                                        }),
                                        Err(_) => {
                                            let _ = leader.complete(Err(
                                                crate::shared_build::SharedBuildTerminalFailure::Internal,
                                            ));
                                            return reject(
                                                &mut output,
                                                "shared-build-state",
                                                "shared build succeeded with invalid result metadata",
                                            );
                                        }
                                    }
                                }
                                crate::persistence::SharedBuildState::Failed => {
                                    let _ = leader.complete(Err(
                                        crate::shared_build::SharedBuildTerminalFailure::Backend,
                                    ));
                                    return reject(
                                        &mut output,
                                        "shared-build-state",
                                        "shared BuildDerivation execution failed",
                                    );
                                }
                                crate::persistence::SharedBuildState::Claimed
                                | crate::persistence::SharedBuildState::Running
                                | crate::persistence::SharedBuildState::Collecting => {
                                    let result = match wait_for_shared_build_terminal(
                                        database,
                                        derivation_path,
                                    )
                                    .map_err(|error| {
                                        io::Error::new(
                                            error.kind(),
                                            format!("shared build terminal wait failed: {error}"),
                                        )
                                    })
                                    .and_then(|_| {
                                        crate::persistence::read_shared_build(
                                            database,
                                            derivation_path,
                                        )
                                        .map_err(|error| {
                                            io::Error::other(format!(
                                                "shared build read failed: {:?}",
                                                error.failure()
                                            ))
                                        })?
                                        .ok_or_else(|| io::Error::other("shared build is missing"))
                                    })
                                    .and_then(|build| durable_shared_build_result(&build))
                                    {
                                        Ok(result) => result,
                                        Err(error) => {
                                            let _ = leader.complete(Err(
                                                crate::shared_build::SharedBuildTerminalFailure::Internal,
                                            ));
                                            return reject(
                                                &mut output,
                                                "shared-build-state",
                                                &error.to_string(),
                                            );
                                        }
                                    };
                                    leader.complete(Ok(result)).map_err(|_| {
                                        io::Error::other("shared BuildDerivation execution failed")
                                    })
                                }
                            }
                        } else {
                            durable_execution_owned.set(true);
                            let queue_started = std::time::Instant::now();
                            crate::service::metrics::shared_build_enqueued();
                            tracing::info!(
                                event = "shared_build.queue.enqueued",
                                backend_name = selected_target.name(),
                                backend_kind = selected_target.kind().as_str(),
                                "shared build enqueued"
                            );
                            if let Err(error) = crate::persistence::enqueue_shared_build(
                                database,
                                derivation_path,
                                quota_subject,
                                scheduling_limits.maximum_queued_builds(),
                            )
                            .map_err(|error| {
                                tracing::warn!(
                                    event = "shared_build.scheduling.operation_failed",
                                    operation = "enqueue",
                                    failure = ?error.failure(),
                                    "shared build scheduling operation failed"
                                );
                                io::Error::other(shared_build_error_message(&error))
                            })
                            .and_then(|_| {
                                crate::service::activity::run("queue", || shared_build_scheduler
                                    .wait_for_admission(derivation_path))
                                    .map_err(|error| {
                                        tracing::warn!(
                                            event = "shared_build.scheduling.operation_failed",
                                            operation = "admission-wait",
                                            diagnostic = %error,
                                            "shared build scheduling operation failed"
                                        );
                                        error
                                    })
                            }) {
                                tracing::warn!(
                                    event = "shared_build.scheduling.failed",
                                    diagnostic = %error,
                                    "shared build scheduling failed"
                                );
                                crate::service::metrics::shared_build_left_queue();
                                let _ = crate::persistence::complete_shared_build_failure(
                                    database,
                                    derivation_path,
                                    "scheduling-failure",
                                    &serde_json::json!({"failure": execution_error_reason(&error)}),
                                    output_retention.duration(),
                                );
                                let _ = leader.complete(Err(
                                    crate::shared_build::SharedBuildTerminalFailure::Internal,
                                ));
                                return reject(
                                    &mut output,
                                    "shared-build-scheduling",
                                    "shared build scheduling failed",
                                );
                            }
                            crate::service::metrics::shared_build_admitted(queue_started.elapsed());
                            tracing::info!(
                                event = "shared_build.queue.admitted",
                                backend_name = selected_target.name(),
                                backend_kind = selected_target.kind().as_str(),
                                duration_ms = queue_started.elapsed().as_millis(),
                                "shared build left queue for execution"
                            );
                            let substitution_started = std::time::Instant::now();
                            let result = match crate::service::activity::run("substitute", || Ok(substitute_build_outputs(
                                store_substitution,
                                admitted.expected_outputs(),
                            )))? {
                                Some(result) => {
                                    crate::service::metrics::cache_substitution_finished(
                                        substitution_started.elapsed(),
                                        "hit",
                                    );
                                    tracing::info!(
                                        event = "worker.build_derivation.substituted",
                                        output_count = result.outputs().len(),
                                        "BuildDerivation outputs substituted through gateway store"
                                    );
                                    Ok(result)
                                }
                                None => {
                                    crate::service::metrics::cache_substitution_finished(
                                        substitution_started.elapsed(),
                                        "miss",
                                    );
                                    let permit = build_executor
                                        .reserve_target(admitted.system(), &required_features)?
                                        .ok_or_else(|| io::Error::other(
                                            "backend capacity reservation is unavailable",
                                        ))?;
                                    let selected_target = permit.target().clone();
                                    execution.set_target_name(selected_target.name())?;
                                    let backend_execution_id = build_executor.execution_id(
                                        &selected_target,
                                        shared_build_key.as_bytes(),
                                    )?;
                                    crate::persistence::reassign_running_shared_build(
                                        database,
                                        derivation_path,
                                        selected_target.name(),
                                        selected_target.kind(),
                                        selected_target.capabilities(),
                                        backend_execution_id.as_deref(),
                                    )
                                    .map_err(|error| io::Error::other(format!(
                                        "shared build backend assignment failed: {:?}",
                                        error.failure(),
                                    )))?;
                                    tracing::info!(
                                        event = "shared_build.backend.assigned",
                                        backend_name = selected_target.name(),
                                        backend_kind = selected_target.kind().as_str(),
                                        selection_priority = selected_target.selection_priority(),
                                        "shared build assigned available backend capacity"
                                    );
                                    crate::service::activity::run("execute", || build_executor.execute_reserved(
                            &execution,
                            permit,
                            &mut |chunk| {
                                if selected_target.kind() != crate::backend::BackendKind::Nomad {
                                    tracing::debug!(event = "server.build.output", output = %String::from_utf8_lossy(chunk));
                                }
                                if requester_detached.get() {
                                    return Ok(());
                                }
                                match nix_worker_protocol::write_stderr_frame(
                                    &mut output,
                                    nix_worker_protocol::StderrFrame::Next {
                                        message: chunk.to_vec(),
                                    },
                                )
                                .and_then(|_| output.flush())
                                {
                                    Ok(()) => Ok(()),
                                    Err(_error)
                                        if running_disconnect_policy
                                            == crate::service::deployment::RunningDisconnectPolicy::DetachAndFinish =>
                                    {
                                        requester_detached.set(true);
                                        tracing::info!(
                                            event = "worker.build_derivation.requester_detached",
                                            running_disconnect_policy = running_disconnect_policy.as_str(),
                                            "running build detached from requester"
                                        );
                                        Ok(())
                                    }
                                    Err(error) => Err(error),
                                }
                            },
                            &mut || {
                                let disconnected = requester_disconnected(&mut cancellation_input)?;
                                if disconnected
                                    && running_disconnect_policy
                                        == crate::service::deployment::RunningDisconnectPolicy::DetachAndFinish
                                {
                                    requester_detached.set(true);
                                    return Ok(false);
                                }
                                Ok(disconnected)
                            },
                        ))
                                }
                            };
                            match result {
                                Ok(result) => {
                                    crate::service::metrics::build_execution_finished(
                                        execution_started.elapsed(),
                                        "succeeded",
                                        None,
                                    );
                                    leader.complete(Ok(result)).map_err(|_| {
                                        io::Error::other("shared BuildDerivation execution failed")
                                    })
                                }
                                Err(error) => {
                                    crate::service::metrics::build_execution_finished(
                                        execution_started.elapsed(),
                                        "failed",
                                        Some(execution_error_reason(&error)),
                                    );
                                    let timeout_phase = execution_timeout_phase(&error);
                                    let _ = crate::persistence::complete_shared_build_failure(
                                        database,
                                        derivation_path,
                                        "backend-failure",
                                        &serde_json::json!({
                                            "reason": execution_error_reason(&error),
                                            "timeout_phase": timeout_phase,
                                        }),
                                        output_retention.duration(),
                                    );
                                    let failure = if error.kind() == io::ErrorKind::Unsupported {
                                        crate::shared_build::SharedBuildTerminalFailure::BackendUnavailable
                                    } else {
                                        crate::shared_build::SharedBuildTerminalFailure::Backend
                                    };
                                    let _ = leader.complete(Err(failure));
                                    Err(error)
                                }
                            }
                        }
                    }
                    crate::shared_build::SharedBuildAccess::Follower(follower) => {
                        let live_log_queue_bytes = build_executor.compatible_live_log_queue_bytes(
                            admitted.system(),
                            &required_features,
                        );
                        let mut live_logs = follower.subscribe_logs(live_log_queue_bytes);
                        crate::service::metrics::shared_build_follower();
                        tracing::info!(
                            event = "shared_build.coalescing.follower",
                            backend_name = selected_target.name(),
                            backend_kind = selected_target.kind().as_str(),
                            "request joined shared-build leader"
                        );
                        nix_worker_protocol::write_stderr_frame(
                            &mut output,
                            nix_worker_protocol::StderrFrame::Next {
                                message: b"identical build already in progress\n".to_vec(),
                            },
                        )?;
                        output.flush()?;
                        let result = crate::service::activity::run("follow", || follower
                            .wait_timeout_with_logs(
                                execution.timeout(),
                                &mut live_logs,
                                &mut |chunk: &[u8]| -> io::Result<()> {
                                    if requester_detached.get() {
                                        return Ok(());
                                    }
                                    match nix_worker_protocol::write_stderr_frame(
                                        &mut output,
                                        nix_worker_protocol::StderrFrame::Next {
                                            message: chunk.to_vec(),
                                        },
                                    )
                                    .and_then(|_| output.flush())
                                    {
                                        Ok(()) => Ok(()),
                                        Err(_error)
                                            if running_disconnect_policy
                                                == crate::service::deployment::RunningDisconnectPolicy::DetachAndFinish =>
                                        {
                                            requester_detached.set(true);
                                            Ok(())
                                        }
                                        Err(error) => Err(error),
                                    }
                                },
                            ))?
                            .ok_or_else(|| {
                                io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    "shared BuildDerivation follower wait timed out",
                                )
                            })?
                            .map_err(|failure| {
                                match failure {
                            crate::shared_build::SharedBuildTerminalFailure::BackendUnavailable => {
                                io::Error::new(
                                    io::ErrorKind::Unsupported,
                                    "shared BuildDerivation execution is unavailable",
                                )
                            }
                                crate::shared_build::SharedBuildTerminalFailure::Backend
                                | crate::shared_build::SharedBuildTerminalFailure::Internal => {
                                    io::Error::other("shared BuildDerivation execution failed")
                                }
                            }
                            })?;
                        wait_for_shared_build_terminal(database, derivation_path)?;
                        Ok(result)
                    }
                };
                let result = match shared_result {
                    Ok(result) => result,
                    Err(error) => {
                        let unavailable = error.kind() == io::ErrorKind::Unsupported;
                        crate::service::metrics::build_request_finished(
                            request_started.elapsed(),
                            "failed",
                            Some(if unavailable {
                                "unavailable"
                            } else {
                                execution_error_reason(&error)
                            }),
                        );
                        tracing::error!(
                            event = if unavailable {
                                "worker.build_derivation.execution_unavailable"
                            } else {
                                "worker.build_derivation.failed"
                            },
                            reason = execution_error_reason(&error),
                            timeout_phase = execution_timeout_phase(&error),
                            "BuildDerivation execution failed"
                        );
                        if let Err(release_error) = release_attached_request_leases(
                            store_retention,
                            database,
                            session_id,
                            &request_id,
                        ) {
                            return reject(
                                &mut output,
                                "request-lease-release",
                                release_error_message(&release_error),
                            );
                        }
                        if requester_detached.get()
                            || error.kind() == io::ErrorKind::ConnectionAborted
                        {
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
                let validation_started = std::time::Instant::now();
                let output_paths = crate::service::activity::run("validate-outputs", || validate_build_outputs(&result, &admitted, store_export));
                crate::service::metrics::store_validation_finished(
                    validation_started.elapsed(),
                    if output_paths.is_ok() {
                        "succeeded"
                    } else {
                        "failed"
                    },
                    if admitted
                        .output_authorities()
                        .iter()
                        .any(|authority| !authority.hash_algorithm().is_empty())
                    {
                        "fixed_output"
                    } else {
                        "input_addressed"
                    },
                );
                let output_paths = match output_paths {
                    Ok(paths) => {
                        if durable_execution_owned.get() {
                            let build = crate::persistence::read_shared_build(
                                database,
                                derivation_path,
                            )
                            .map_err(|error| io::Error::other(shared_build_error_message(&error)))?
                            .ok_or_else(|| io::Error::other("shared build is missing"))?;
                            if build.state == crate::persistence::SharedBuildState::Running
                                && let Err(error) = crate::persistence::collect_shared_build(
                                    database,
                                    derivation_path,
                                )
                            {
                                return reject(
                                    &mut output,
                                    "shared-build-state",
                                    shared_build_error_message(&error),
                                );
                            }
                        }
                        paths
                    }
                    Err(error) => {
                        if durable_execution_owned.get() {
                            let _ = crate::persistence::complete_shared_build_failure(
                                database,
                                derivation_path,
                                "output-validation-failure",
                                &serde_json::json!({"reason": execution_error_reason(&error)}),
                                output_retention.duration(),
                            );
                        }
                        tracing::error!(
                            event = "worker.build_derivation.output_validation_failed",
                            reason = execution_error_reason(&error),
                            diagnostic = %error,
                            "BuildDerivation output validation failed"
                        );
                        if let Err(release_error) = release_attached_request_leases(
                            store_retention,
                            database,
                            session_id,
                            &request_id,
                        ) {
                            return reject(
                                &mut output,
                                "request-lease-release",
                                release_error_message(&release_error),
                            );
                        }
                        if requester_detached.get() {
                            return Ok(());
                        }
                        return reject(
                            &mut output,
                            "build-derivation-failed",
                            "BuildDerivation execution failed",
                        );
                    }
                };
                let output_leases = output_paths
                    .iter()
                    .map(|path| (output_lease_id(), path.clone()))
                    .collect::<Vec<_>>();
                let output_entries = output_leases
                    .iter()
                    .map(|(lease_id, store_path)| {
                        crate::store::retention::RetentionEntry::new(lease_id, store_path)
                    })
                    .collect::<Vec<_>>();
                let retained_outputs = match store_retention.retain(&output_entries) {
                    Ok(retained) => {
                        retention_batch_event(
                            "retain",
                            "output",
                            output_entries.len(),
                            "succeeded",
                            None,
                        );
                        retained
                    }
                    Err(_) => {
                        retention_batch_event(
                            "retain",
                            "output",
                            output_entries.len(),
                            "failed",
                            Some("helper"),
                        );
                        if let Err(release_error) = release_attached_request_leases(
                            store_retention,
                            database,
                            session_id,
                            &request_id,
                        ) {
                            return reject(
                                &mut output,
                                "request-lease-release",
                                release_error_message(&release_error),
                            );
                        }
                        return reject(
                            &mut output,
                            "gateway-store-retention",
                            "gateway store retention failed",
                        );
                    }
                };
                if crate::persistence::create_request_output_leases(
                    database,
                    &request_id,
                    output_retention.duration(),
                    &output_leases,
                )
                .is_err()
                {
                    if durable_execution_owned.get() {
                        let _ = crate::persistence::complete_shared_build_failure(
                            database,
                            derivation_path,
                            "output-retention-failure",
                            &serde_json::json!({"stage": "lease"}),
                            output_retention.duration(),
                        );
                    }
                    if store_retention.rollback(&retained_outputs).is_err() {
                        retention_batch_event(
                            "rollback",
                            "output",
                            output_entries.len(),
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
                        "output",
                        output_entries.len(),
                        "succeeded",
                        None,
                    );
                    if let Err(release_error) = release_attached_request_leases(
                        store_retention,
                        database,
                        session_id,
                        &request_id,
                    ) {
                        return reject(
                            &mut output,
                            "request-lease-release",
                            release_error_message(&release_error),
                        );
                    }
                    return reject(
                        &mut output,
                        "store-lease-state",
                        "store lease state operation failed",
                    );
                }
                if let Err(error) = release_attached_request_leases(
                    store_retention,
                    database,
                    session_id,
                    &request_id,
                ) {
                    return reject(
                        &mut output,
                        "request-lease-release",
                        release_error_message(&error),
                    );
                }
                let build_already_succeeded =
                    crate::persistence::read_shared_build(database, derivation_path)
                        .ok()
                        .flatten()
                        .is_some_and(|build| {
                            build.state == crate::persistence::SharedBuildState::Succeeded
                        });
                if durable_execution_owned.get()
                    && !build_already_succeeded
                    && let Err(error) = crate::persistence::complete_shared_build_success(
                        database,
                        derivation_path,
                        &serde_json::json!({
                        "status": match result.status() {
                            BuildStatus::Built => "built",
                            BuildStatus::AlreadyValid => "already-valid",
                        },
                        "outputs": result.outputs().iter().map(|(name, path)| {
                            serde_json::json!({
                                "name": String::from_utf8_lossy(name),
                                "path": String::from_utf8_lossy(path),
                            })
                        }).collect::<Vec<_>>(),
                        }),
                        output_retention.duration(),
                    )
                {
                    return reject(
                        &mut output,
                        "shared-build-state",
                        &format!("shared build completion failed: {:?}", error.failure()),
                    );
                }
                if durable_execution_owned.get()
                    && !build_already_succeeded
                    && let Some(publisher) = cache_publisher.cloned()
                {
                    let publication_outputs = output_paths.clone();
                    std::thread::spawn(move || {
                        let started = std::time::Instant::now();
                        match publisher.publish(&publication_outputs) {
                            Ok(()) => {
                                crate::service::metrics::cache_publication_finished(
                                    started.elapsed(),
                                    "succeeded",
                                );
                                tracing::info!(
                                    event = "worker.build_derivation.cache_published",
                                    output_count = publication_outputs.len(),
                                    "BuildDerivation outputs published"
                                )
                            }
                            Err(_) => {
                                crate::service::metrics::cache_publication_finished(
                                    started.elapsed(),
                                    "failed",
                                );
                                tracing::warn!(
                                    event = "worker.build_derivation.cache_publication_failed",
                                    output_count = publication_outputs.len(),
                                    "BuildDerivation cache publication failed"
                                )
                            }
                        }
                    });
                }
                if !requester_detached.get() {
                    $write_success(
                        &mut output,
                        negotiated.version,
                        result.status() == BuildStatus::AlreadyValid,
                    )?;
                }
                crate::service::metrics::build_request_finished(
                    request_started.elapsed(),
                    "succeeded",
                    None,
                );
                tracing::info!(
                    event = "worker.build_derivation.completed",
                    output_count = result.outputs().len(),
                    status = ?result.status(),
                    "BuildDerivation execution completed"
                );

        }};
    }

    loop {
        let receiving = tracing::enabled!(tracing::Level::TRACE).then(std::time::Instant::now);
        let operation = reader.read_operation();
        tracing::trace!(event = "worker.operation.received", operation = ?operation.as_ref().ok(), elapsed_us = receiving.map(|start| start.elapsed().as_micros() as u64), success = operation.is_ok());
        let _operation =
            tracing::trace_span!("worker.operation", operation = ?operation.as_ref().ok())
                .entered();
        match operation {
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
                let request_started = std::time::Instant::now();
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
                        tracing::warn!(
                            event = "worker.build_derivation.rejected",
                            phase = "decode",
                            diagnostic = %error,
                            "BuildDerivation request rejected"
                        );
                        return reject(
                            &mut output,
                            "invalid-build-derivation",
                            "invalid BuildDerivation request",
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            event = "worker.build_derivation.rejected",
                            phase = "decode",
                            diagnostic = %error,
                            "BuildDerivation request rejected"
                        );
                        return reject(
                            &mut output,
                            "invalid-build-derivation",
                            "invalid BuildDerivation request",
                        );
                    }
                };
                let admitted = match BuildRequest::from_worker_request(&request, backend_targets) {
                    Ok(admitted) => admitted,
                    Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                        tracing::warn!(
                            event = "worker.build_derivation.rejected",
                            phase = "admission",
                            diagnostic = %error,
                            "BuildDerivation request rejected"
                        );
                        return reject(
                            &mut output,
                            "unsupported-build-derivation",
                            "unsupported BuildDerivation request",
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            event = "worker.build_derivation.rejected",
                            phase = "admission",
                            diagnostic = %error,
                            "BuildDerivation request rejected"
                        );
                        return reject(
                            &mut output,
                            "invalid-build-derivation",
                            "invalid BuildDerivation request",
                        );
                    }
                };
                execute_admitted_build!(
                    admitted,
                    request_started,
                    request.build_mode(),
                    request.platform(),
                    nix_worker_protocol::write_build_derivation_success_response,
                );
            }
            Ok(WorkerOperation::BuildPathsWithResults) => {
                let request_started = std::time::Instant::now();
                let request = match reader.complete_build_paths_with_results() {
                    Ok(request) if request.build_mode() == 0 && request.targets().len() == 1 => {
                        request
                    }
                    Ok(_) => {
                        return reject(
                            &mut output,
                            "unsupported-build-paths-with-results",
                            "unsupported BuildPathsWithResults request",
                        );
                    }
                    Err(_) => {
                        return reject(
                            &mut output,
                            "invalid-build-paths-with-results",
                            "invalid BuildPathsWithResults request",
                        );
                    }
                };
                let target = &request.targets()[0];
                if !target.ends_with(b".drv") && !target.contains(&b'!') {
                    let valid_paths =
                        match store_query.query_valid_paths([target.clone()].as_slice(), false) {
                            Ok(paths) => paths,
                            Err(error) => {
                                tracing::error!(
                                    event = "worker.build_paths_with_results.validity_failed",
                                    reason = error.to_string(),
                                    "BuildPathsWithResults target validity query failed"
                                );
                                return reject(
                                    &mut output,
                                    "build-paths-with-results-validity",
                                    "BuildPathsWithResults target validity query failed",
                                );
                            }
                        };
                    if valid_paths == [target.clone()] {
                        nix_worker_protocol::write_build_paths_with_results_success_response(
                            &mut output,
                            negotiated.version,
                            [(target.as_slice(), true)],
                        )?;
                        crate::service::metrics::build_request_finished(
                            request_started.elapsed(),
                            "succeeded",
                            None,
                        );
                        tracing::info!(
                            event = "worker.build_paths_with_results.completed",
                            status = "already-valid",
                            "BuildPathsWithResults request completed"
                        );
                        continue;
                    }
                    return reject(
                        &mut output,
                        "invalid-build-paths-with-results",
                        "invalid BuildPathsWithResults request",
                    );
                }
                let derivation_path = target
                    .split(|byte| *byte == b'!')
                    .next()
                    .unwrap_or(target.as_slice());
                let derivation_path = match std::str::from_utf8(derivation_path) {
                    Ok(path) => std::path::Path::new(path),
                    Err(_) => {
                        return reject(
                            &mut output,
                            "invalid-build-paths-with-results",
                            "invalid BuildPathsWithResults request",
                        );
                    }
                };
                let admitted =
                    match BuildRequest::load_stored(derivation_path, store_export, backend_targets)
                    {
                        Ok(admitted) => admitted,
                        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                            return reject(
                                &mut output,
                                "unsupported-build-paths-with-results",
                                "unsupported BuildPathsWithResults request",
                            );
                        }
                        Err(error) => {
                            let dependency_result =
                                crate::build::stored_load_dependency_result(&error);
                            let dependency_target = dependency_result
                                .as_ref()
                                .map(|result| String::from_utf8_lossy(&result.target).into_owned())
                                .unwrap_or_else(|| "none".to_owned());
                            tracing::error!(
                                event = "gateway.stored_build.failed",
                                phase = crate::build::stored_load_failure_phase(&error)
                                    .map(crate::build::StoredLoadFailurePhase::as_str)
                                    .unwrap_or("unknown"),
                                dependency_phase =
                                    crate::build::stored_load_dependency_phase(&error)
                                        .map(nix_worker_protocol::BuildPathsFailurePhase::as_str)
                                        .unwrap_or("none"),
                                dependency_target,
                                dependency_status =
                                    dependency_result.as_ref().map(|result| result.status),
                                dependency_category = dependency_result
                                    .as_ref()
                                    .map(|result| result.category)
                                    .unwrap_or("none"),
                                "stored build request loading failed"
                            );
                            return reject(
                                &mut output,
                                "invalid-build-paths-with-results",
                                "invalid BuildPathsWithResults request",
                            );
                        }
                    };
                let target = target.clone();
                let requested_system = admitted.system().as_bytes().to_vec();
                execute_admitted_build!(
                    admitted,
                    request_started,
                    request.build_mode(),
                    requested_system.as_slice(),
                    |output, version, already_valid| {
                        nix_worker_protocol::write_build_paths_with_results_success_response(
                            output,
                            version,
                            [(target.as_slice(), already_valid)],
                        )
                    },
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
                let info = match crate::store::export::query_path_info(path, store_export) {
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
                let flushing = std::time::Instant::now();
                output.flush()?;
                tracing::debug!(
                    event = "worker.query_path_info.response_flushed",
                    elapsed_us = flushing.elapsed().as_micros() as u64
                );
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
                let (verified, first_byte_us, write_elapsed_us, write_call_count) = {
                    let mut measured_output = NarResponseWriter::new(&mut output);
                    let verified =
                        match crate::store::export::export_verified_nar_with_limits_and_rate(
                            path,
                            &mut measured_output,
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
                    (
                        verified,
                        measured_output.first_byte_us(),
                        measured_output.elapsed_us(),
                        measured_output.write_call_count(),
                    )
                };
                let flushing = std::time::Instant::now();
                output.flush()?;
                tracing::debug!(
                    event = "worker.nar_from_path.transport.completed",
                    path = %path.display(),
                    nar_size = verified.nar_size,
                    first_byte_us,
                    write_elapsed_us,
                    write_call_count,
                    flush_elapsed_us = flushing.elapsed().as_micros() as u64
                );
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
                                &store_directory,
                                staging_directory,
                                info.nar_size(),
                            )
                        {
                            disk_reserve_rejected("transfer", disk_reserve, error);
                            disk_rejection = Some(error);
                            return Err(io::Error::other("disk reserve rejected"));
                        }
                        let mut limited = crate::service::transfer_limits::LimitedReader::with_rate(
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
            Ok(WorkerOperation::QueryMissing) => {
                let request = match reader.complete_query_missing() {
                    Ok(request) => request,
                    Err(error) => {
                        tracing::error!(
                            event = "worker.operation.rejected",
                            rejection = "invalid-query-missing",
                            reason = error.to_string(),
                            "QueryMissing request rejected"
                        );
                        return reject(
                            &mut output,
                            "invalid-query-missing",
                            "invalid QueryMissing request",
                        );
                    }
                };
                let requested_count = request.targets().len();
                let missing = match store_query.query_missing(request.targets()) {
                    Ok(missing) => missing,
                    Err(error) => {
                        tracing::error!(
                            event = "worker.query_missing.failed",
                            reason = error.to_string(),
                            "gateway store QueryMissing failed"
                        );
                        return reject(
                            &mut output,
                            "query-missing-store-failure",
                            "QueryMissing store query failed",
                        );
                    }
                };
                output.write_all(&nix_worker_protocol::STDERR_LAST.to_le_bytes())?;
                nix_worker_protocol::write_query_missing_response(
                    &mut output,
                    &missing.will_build,
                    &missing.will_substitute,
                    &missing.unknown,
                    missing.download_size,
                    missing.nar_size,
                )?;
                output.flush()?;
                tracing::info!(
                    event = "worker.query_missing.completed",
                    requested_count,
                    will_build_count = missing.will_build.len(),
                    will_substitute_count = missing.will_substitute.len(),
                    unknown_count = missing.unknown.len(),
                    "QueryMissing request completed"
                );
            }
            Ok(WorkerOperation::QueryValidPaths) => {
                let decoding =
                    tracing::enabled!(tracing::Level::TRACE).then(std::time::Instant::now);
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
                tracing::trace!(
                    event = "worker.query_valid_paths.decoded",
                    elapsed_us = decoding.map(|start| start.elapsed().as_micros() as u64),
                    requested_count
                );
                let querying =
                    tracing::enabled!(tracing::Level::TRACE).then(std::time::Instant::now);
                let valid_paths =
                    match store_query.query_valid_paths(request.paths(), request.substitute()) {
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
                tracing::trace!(
                    event = "worker.query_valid_paths.queried",
                    elapsed_us = querying.map(|start| start.elapsed().as_micros() as u64),
                    valid_count = valid_paths.len()
                );
                let responding =
                    tracing::enabled!(tracing::Level::TRACE).then(std::time::Instant::now);
                output.write_all(&nix_worker_protocol::STDERR_LAST.to_le_bytes())?;
                write_query_valid_paths_response(&mut output, &valid_paths)?;
                tracing::trace!(
                    event = "worker.query_valid_paths.written",
                    elapsed_us = responding.map(|start| start.elapsed().as_micros() as u64)
                );
                output.flush()?;
                tracing::trace!(
                    event = "worker.query_valid_paths.flushed",
                    elapsed_us = responding.map(|start| start.elapsed().as_micros() as u64)
                );
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
            Ok(operation) => {
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
        }
    }
}

struct NarResponseWriter<'a, W> {
    output: &'a mut W,
    started: std::time::Instant,
    first_byte_us: Option<u64>,
    write_elapsed_us: u64,
    write_call_count: u64,
}

impl<'a, W> NarResponseWriter<'a, W> {
    fn new(output: &'a mut W) -> Self {
        Self {
            output,
            started: std::time::Instant::now(),
            first_byte_us: None,
            write_elapsed_us: 0,
            write_call_count: 0,
        }
    }

    fn first_byte_us(&self) -> Option<u64> {
        self.first_byte_us
    }

    fn elapsed_us(&self) -> u64 {
        self.write_elapsed_us
    }

    fn write_call_count(&self) -> u64 {
        self.write_call_count
    }
}

impl<W: Write> Write for NarResponseWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if !buffer.is_empty() && self.first_byte_us.is_none() {
            self.first_byte_us = Some(self.started.elapsed().as_micros() as u64);
        }
        let started = std::time::Instant::now();
        let written = self.output.write(buffer)?;
        self.write_elapsed_us += started.elapsed().as_micros() as u64;
        self.write_call_count += 1;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

fn run_cached_output_phase<T>(
    phase: &'static str,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let started = std::time::Instant::now();
    let result = operation();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => tracing::debug!(event = "cached_output.phase.completed", phase, elapsed_ms),
        Err(error) => tracing::warn!(
            event = "cached_output.phase.failed",
            phase,
            elapsed_ms,
            error_kind = ?error.kind()
        ),
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn return_cached_outputs(
    request: &BuildRequest,
    database: &crate::persistence::Database,
    store_query: &mut dyn QueryValidPathsStore,
    store_export: &mut dyn crate::store::export::StoreExportBackend,
    store_retention: &mut dyn crate::store::retention::StoreRetentionBackend,
    retention: Duration,
    audit_subject: &str,
    quota_subject: &str,
) -> io::Result<bool> {
    let derivation_path = std::str::from_utf8(request.derivation_path())
        .map_err(|_| io::Error::other("invalid derivation path"))?;
    let Some(build) = run_cached_output_phase("lookup", || {
        crate::persistence::read_shared_build(database, derivation_path)
            .map_err(|_| io::Error::other("cached build lookup failed"))
    })?
    else {
        return Ok(false);
    };
    if build.state != crate::persistence::SharedBuildState::Succeeded {
        return Ok(false);
    }
    if build.request_digest != request.shared_build_digest()
        || build.build_request.as_ref() != Some(request)
    {
        return Err(io::Error::other("cached build request identity mismatch"));
    }
    let result = durable_shared_build_result(&build)?;
    if result.outputs() != request.expected_outputs() {
        return Err(io::Error::other("cached build output identity mismatch"));
    }
    let output_leases = result
        .outputs()
        .iter()
        .map(|(_, path)| {
            std::str::from_utf8(path)
                .map(|path| (output_lease_id(), path.to_owned()))
                .map_err(|_| io::Error::other("invalid cached output path"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let entries = output_leases
        .iter()
        .map(|(id, path)| crate::store::retention::RetentionEntry::new(id, path))
        .collect::<Vec<_>>();
    let expected_paths = request
        .expected_outputs()
        .iter()
        .map(|(_, path)| path.clone())
        .collect::<Vec<_>>();
    let valid_paths = run_cached_output_phase("validity", || {
        store_query.query_valid_paths(&expected_paths, false)
    })?;
    if valid_paths.len() != expected_paths.len()
        || expected_paths
            .iter()
            .any(|path| !valid_paths.contains(path))
    {
        return Ok(false);
    }
    // Roots protect the output closure while it is validated and retained for retrieval.
    let retained = run_cached_output_phase("retention", || store_retention.retain(&entries))?;
    let persisted = (|| {
        run_cached_output_phase("integrity", || {
            validate_build_outputs(&result, request, store_export)
        })?;
        let request_id = build_request_id();
        run_cached_output_phase("request", || {
            crate::persistence::create_build_request(
                database,
                &request_id,
                derivation_path,
                request.system(),
                audit_subject,
                quota_subject,
            )
            .map_err(|_| io::Error::other("cached build request persistence failed"))
        })?;
        run_cached_output_phase("lease", || {
            crate::persistence::create_request_output_leases(
                database,
                &request_id,
                retention,
                &output_leases,
            )
            .map_err(|_| io::Error::other("cached output retention persistence failed"))
        })?;
        Ok::<(), io::Error>(())
    })();
    if let Err(error) = persisted {
        store_retention.rollback(&retained)?;
        return Err(error);
    }
    Ok(true)
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

fn output_lease_id() -> String {
    format!(
        "output-{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        OUTPUT_LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
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

fn disk_reserve_error(error: crate::service::disk_reserve::AdmissionFailure) -> &'static str {
    match error.reason() {
        crate::service::disk_reserve::RejectionReason::InsufficientSpace => {
            "gateway disk reserve exceeded"
        }
        crate::service::disk_reserve::RejectionReason::ProbeFailed
        | crate::service::disk_reserve::RejectionReason::ArithmeticOverflow => {
            "gateway disk reserve check failed"
        }
    }
}

fn disk_reserve_rejected(
    operation: &'static str,
    reserve: crate::service::disk_reserve::DiskReserve,
    error: crate::service::disk_reserve::AdmissionFailure,
) {
    let reason = match error.reason() {
        crate::service::disk_reserve::RejectionReason::InsufficientSpace => "insufficient-space",
        crate::service::disk_reserve::RejectionReason::ProbeFailed => "probe-failed",
        crate::service::disk_reserve::RejectionReason::ArithmeticOverflow => "arithmetic-overflow",
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
    store_retention: &mut dyn crate::store::retention::StoreRetentionBackend,
    database: &crate::persistence::Database,
    session_id: &str,
    request_id: &str,
) -> io::Result<()> {
    let released =
        crate::persistence::detach_request_and_release_leases(database, session_id, request_id)
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
    store_retention: &mut dyn crate::store::retention::StoreRetentionBackend,
    database: &crate::persistence::Database,
    request_id: &str,
) -> io::Result<()> {
    let released = crate::persistence::release_unattached_request_leases(database, request_id)
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
    store_retention: &mut dyn crate::store::retention::StoreRetentionBackend,
    leases: &[crate::persistence::StoreLeaseRecord],
) -> io::Result<()> {
    let entries = leases
        .iter()
        .map(|lease| {
            crate::store::retention::ReleasedRetentionEntry::new(
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

fn wait_for_shared_build_terminal(
    database: &crate::persistence::Database,
    derivation_path: &str,
) -> io::Result<()> {
    crate::service::activity::run("await-terminal", || {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let build = crate::persistence::read_shared_build(database, derivation_path)
                .map_err(|error| io::Error::other(shared_build_error_message(&error)))?
                .ok_or_else(|| io::Error::other("shared build is missing"))?;
            match build.state {
                crate::persistence::SharedBuildState::Succeeded => return Ok(()),
                crate::persistence::SharedBuildState::Failed => {
                    return Err(io::Error::other("shared BuildDerivation execution failed"));
                }
                crate::persistence::SharedBuildState::Claimed
                | crate::persistence::SharedBuildState::Running
                | crate::persistence::SharedBuildState::Collecting => {}
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "shared build completion timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })
}

fn substitute_build_outputs(
    store: &mut dyn crate::store::substitution::StoreSubstitutionBackend,
    expected_outputs: &[(Vec<u8>, Vec<u8>)],
) -> Option<crate::backend::BuildResult> {
    for (_, path) in expected_outputs {
        if let Err(error) = store.ensure_path(path) {
            tracing::info!(
                event = "worker.build_derivation.substitution_missed",
                reason = execution_error_reason(&error),
                "BuildDerivation output substitution did not complete"
            );
            return None;
        }
    }
    crate::backend::BuildResult::new(
        crate::backend::BuildStatus::AlreadyValid,
        expected_outputs.to_vec(),
        crate::backend::OutputTrust::TrustedExecutor,
    )
    .ok()
}

fn validate_build_outputs(
    result: &crate::backend::BuildResult,
    request: &crate::build::BuildRequest,
    store_export: &mut dyn crate::store::export::StoreExportBackend,
) -> io::Result<Vec<String>> {
    let paths = result
        .outputs()
        .iter()
        .map(|(_, path)| {
            std::str::from_utf8(path)
                .map(str::to_owned)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid output path"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    for (path, authority) in paths.iter().zip(request.output_authorities()) {
        if path.as_bytes() != authority.path() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "build output authority mismatch",
            ));
        }
        let metadata = crate::store::export::validate_store_output(Path::new(path), store_export)?;
        if metadata.content_address.as_deref() != authority.expected_content_address().as_deref() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "build output content address mismatch",
            ));
        }
    }
    Ok(paths)
}

fn durable_shared_build_result(
    build: &crate::persistence::SharedBuild,
) -> io::Result<crate::backend::BuildResult> {
    if build.state != crate::persistence::SharedBuildState::Succeeded {
        return Err(io::Error::other("shared build is not complete"));
    }
    let metadata = build
        .result_metadata
        .as_ref()
        .ok_or_else(|| io::Error::other("shared build result is unavailable"))?;
    let status = match metadata.get("status").and_then(serde_json::Value::as_str) {
        Some("built") => BuildStatus::Built,
        Some("already-valid") => BuildStatus::AlreadyValid,
        _ => return Err(io::Error::other("shared build result is invalid")),
    };
    let outputs = metadata
        .get("outputs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| io::Error::other("shared build result is invalid"))?
        .iter()
        .map(|output| {
            let name = output
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| io::Error::other("shared build result is invalid"))?;
            let path = output
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| io::Error::other("shared build result is invalid"))?;
            Ok::<(Vec<u8>, Vec<u8>), io::Error>((
                name.as_bytes().to_vec(),
                path.as_bytes().to_vec(),
            ))
        })
        .collect::<io::Result<Vec<_>>>()?;
    crate::backend::BuildResult::new(
        status,
        outputs,
        crate::backend::OutputTrust::TrustedExecutor,
    )
    .map_err(|_| io::Error::other("shared build result is invalid"))
}

fn shared_build_error_message(error: &crate::persistence::SharedBuildError) -> &'static str {
    match error.failure() {
        crate::persistence::SharedBuildFailure::Quota => "shared build quota exceeded",
        crate::persistence::SharedBuildFailure::Conflict => "shared build identity conflicts",
        crate::persistence::SharedBuildFailure::Configuration
        | crate::persistence::SharedBuildFailure::Connection
        | crate::persistence::SharedBuildFailure::InvalidState
        | crate::persistence::SharedBuildFailure::Query
        | crate::persistence::SharedBuildFailure::Commit => "shared build state operation failed",
    }
}

fn release_error_message(error: &io::Error) -> &'static str {
    if error.to_string() == "gateway store retention failed" {
        "gateway store retention failed"
    } else {
        "store lease state operation failed"
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

fn execution_timeout_phase(error: &io::Error) -> Option<&'static str> {
    if error.kind() != io::ErrorKind::TimedOut {
        return None;
    }
    match error.to_string().as_str() {
        "backend permit wait timed out" => Some("backend-capacity"),
        "Nomad job execution timed out" => Some("nomad-execution"),
        "Nomad transfer setup timed out" => Some("transfer-setup"),
        "Nomad output collection timed out" => Some("output-collection"),
        "shared BuildDerivation follower wait timed out" => Some("shared-build-follower"),
        _ => Some("unknown"),
    }
}

fn reject(output: &mut impl Write, rejection: &str, message: &str) -> io::Result<()> {
    tracing::error!(
        event = "worker.operation.rejected",
        rejection,
        reason = message,
        "worker operation rejected"
    );
    nix_worker_protocol::write_worker_error(output, message)?;
    output.flush()
}

#[cfg(test)]
mod timeout_diagnostic_tests {
    use super::*;

    #[test]
    fn classifies_backend_capacity_timeout() {
        let error = io::Error::new(io::ErrorKind::TimedOut, "backend permit wait timed out");

        assert_eq!(execution_timeout_phase(&error), Some("backend-capacity"));
    }

    #[test]
    fn classifies_nomad_and_transfer_timeouts() {
        for (message, phase) in [
            ("Nomad job execution timed out", "nomad-execution"),
            ("Nomad transfer setup timed out", "transfer-setup"),
            ("Nomad output collection timed out", "output-collection"),
            (
                "shared BuildDerivation follower wait timed out",
                "shared-build-follower",
            ),
        ] {
            let error = io::Error::new(io::ErrorKind::TimedOut, message);

            assert_eq!(execution_timeout_phase(&error), Some(phase));
        }
    }
}
