mod telemetry;

use std::fs::Permissions;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use telchar::identity::{normalize_requester, IdentityInput};
use telchar::ipc::{IpcEnvelope, IpcListener, RequesterMetadata, IPC_VERSION};

fn main() -> std::process::ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        Some("serve-stdio") => serve_stdio(),
        Some("daemon") => daemon(),
        Some("executor") => executor(),
        _ => smoke(),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("telchar: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn executor() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let telemetry = telemetry::Telemetry::initialize()?;
    let result = run_executor();
    telemetry.shutdown();
    result.map_err(Into::into)
}

fn run_executor() -> io::Result<()> {
    let config =
        telchar::config::ServiceConfig::load().map_err(|_| invalid("database migration failed"))?;
    let database_url = config
        .require_database_url()
        .map_err(|_| invalid("database migration failed"))?
        .to_owned();
    telchar::persistence::migrate(&database_url)
        .map_err(|_| invalid("database migration failed"))?;
    let _ownership =
        telchar::singleton_ownership::SingletonOwnership::acquire_local_executor(&database_url)
            .map_err(|_| invalid("local executor ownership refused"))?;
    let socket = required_path("TELCHAR_EXECUTOR_SOCKET")?;
    let expected_uid = u32_from_env("TELCHAR_EXECUTOR_UID", rustix::process::getuid().as_raw());
    prepare_socket_path(&socket)?;
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, Permissions::from_mode(0o600))?;
    let _socket_guard = SocketGuard(socket);
    let deployment = config.require_deployment()?.clone();
    let executor = Arc::new(Mutex::new(
        telchar::local_executor::executor_from_environment()?,
    ));
    for connection in listener.incoming() {
        let mut stream = connection?;
        if telchar::ipc::authorize_peer(&stream, expected_uid).is_err() {
            continue;
        }
        let mut submit =
            |backend_execution_id: &str,
             specification: &telchar::executor_service::ExecutorSpecification,
             execution: &telchar::persistence::LocalBackendExecution| {
                specification.build.validate_for_execution(&deployment)?;
                if execution.state != telchar::persistence::LocalBackendExecutionState::Accepted {
                    return Ok(());
                }
                let backend_execution_id = backend_execution_id.to_owned();
                let database_url = database_url.clone();
                let specification = specification.clone();
                let executor = Arc::clone(&executor);
                std::thread::spawn(move || {
                    if telchar::persistence::record_local_backend_running(
                        &database_url,
                        &backend_execution_id,
                    )
                    .is_err()
                    {
                        return;
                    }
                    let Ok(request) = telchar::backend::BuildExecution::new(
                        &specification.request_id,
                        &specification.build,
                        Duration::from_secs(specification.timeout_seconds),
                    ) else {
                        return;
                    };
                    let Ok(mut executor) = executor.lock() else {
                        return;
                    };
                    let terminal = match executor.execute_with_logs(
                        &request,
                        &mut |_| Ok(()),
                        &mut || Ok(false),
                    ) {
                        Ok(result) => {
                            let outputs = result
                                .outputs()
                                .iter()
                                .map(|(name, path)| {
                                    let name = String::from_utf8(name.clone()).map_err(|_| ())?;
                                    let path = String::from_utf8(path.clone()).map_err(|_| ())?;
                                    Ok(serde_json::json!({"name": name, "path": path}))
                                })
                                .collect::<Result<Vec<_>, ()>>();
                            match outputs {
                                Ok(outputs) => Some((
                                    telchar::persistence::LocalBackendExecutionState::Succeeded,
                                    "succeeded",
                                    serde_json::json!({
                                        "status": match result.status() {
                                            telchar::backend::BuildStatus::Built => "built",
                                            telchar::backend::BuildStatus::AlreadyValid => "already-valid",
                                        },
                                        "outputs": outputs,
                                    }),
                                )),
                                Err(()) => Some((
                                    telchar::persistence::LocalBackendExecutionState::Failed,
                                    "output-failure",
                                    serde_json::json!({}),
                                )),
                            }
                        }
                        Err(_) => Some((
                            telchar::persistence::LocalBackendExecutionState::Failed,
                            "infrastructure-failure",
                            serde_json::json!({}),
                        )),
                    };
                    if let Some((state, classification, metadata)) = terminal {
                        let _ = telchar::persistence::complete_local_backend_execution(
                            &database_url,
                            &backend_execution_id,
                            state,
                            classification,
                            &metadata,
                        );
                    }
                });
                Ok(())
            };
        if let Err(error) = telchar::executor_service::handle_connection_with_submit(
            &database_url,
            &mut stream,
            &mut submit,
        ) {
            tracing::warn!(
                event = "executor.connection.failed",
                reason = error_reason(&error),
                "local executor connection failed"
            );
        }
    }
    Ok(())
}

fn recover_collecting_build_requests(
    database_url: &str,
    deployment: &telchar::deployment::DeploymentConfig,
    recovered: Vec<telchar::persistence::RecoveredCollectingBuildRequest>,
) -> io::Result<()> {
    let mut store_export = telchar::store_export::backend_from_environment()?;
    let mut store_retention = telchar::store_retention::backend_from_environment()?;
    for recovered in recovered {
        if recovered.backend_result.classification != "succeeded" {
            continue;
        }
        let outputs = recovered
            .backend_result
            .result_metadata
            .get("outputs")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| invalid("collecting result metadata is invalid"))?;
        if outputs.is_empty()
            || outputs.len() > nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_OUTPUTS
        {
            return Err(invalid("collecting result metadata is invalid"));
        }
        let mut leases = Vec::with_capacity(outputs.len());
        for output in outputs {
            let name = output
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| invalid("collecting result metadata is invalid"))?;
            let path = output
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| invalid("collecting result metadata is invalid"))?;
            telchar::store_export::validate_store_output(Path::new(path), store_export.as_mut())?;
            leases.push((
                format!("output-{}-{name}", recovered.execution.attempt.attempt_id),
                path.to_owned(),
            ));
        }
        let entries = leases
            .iter()
            .map(|(lease_id, path)| telchar::store_retention::RetentionEntry::new(lease_id, path))
            .collect::<Vec<_>>();
        let retained = store_retention.retain(&entries)?;
        if telchar::persistence::ensure_request_output_leases(
            database_url,
            &recovered.execution.request.request_id,
            deployment.output_retention().duration(),
            &leases,
        )
        .is_err()
        {
            store_retention.rollback(&retained)?;
            return Err(invalid("collecting output lease recovery failed"));
        }
        telchar::persistence::complete_execution_success(
            database_url,
            &recovered.execution.attempt.attempt_id,
            &recovered.backend_result.result_metadata,
        )
        .map_err(|_| invalid("collecting terminal transition failed"))?;
    }
    Ok(())
}

fn smoke() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let telemetry = telemetry::Telemetry::initialize()?;

    tracing::info!(event = "application.started", "application started");
    if let Some(request_id) = std::env::var_os("TELCHAR_SMOKE_REQUEST_ID") {
        let request_id = request_id.to_string_lossy();
        let request = tracing::info_span!("request", request_id = %request_id);
        let _entered = request.enter();
        tracing::info!(event = "request.started", request_id = %request_id, "request started");
        drop(_entered);
        drop(request);
        opentelemetry::global::meter("telchar")
            .u64_counter("telchar.smoke.events")
            .build()
            .add(1, &[]);
        if std::env::var_os("TELCHAR_SMOKE_ERROR").is_some() {
            tracing::error!(event = "smoke.error", request_id = %request_id, "smoke error");
        }
    }
    println!("{}", nix_worker_protocol::protocol_name());

    telemetry.shutdown();
    Ok(())
}

fn serve_stdio() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let telemetry = telemetry::Telemetry::initialize()?;
    let result = run_frontend();
    if let Err(error) = &result {
        tracing::error!(
            event = "ipc.frontend.failed",
            reason = error_reason(error),
            "stdio frontend failed"
        );
    }
    telemetry.shutdown();
    result.map_err(Into::into)
}

fn run_frontend() -> io::Result<()> {
    let config = telchar::config::ServiceConfig::load()?;
    let socket = config.require_ipc_socket()?;
    let fingerprint = required_string("TELCHAR_AUTHENTICATED_KEY")?;
    let credential_id = format!("ssh-pubkey:{fingerprint}");
    let mapping = config.credential_mapping(&credential_id);
    let requester = normalize_requester(IdentityInput::PublicKey {
        fingerprint,
        audit_subject: mapping.and_then(|mapping| mapping.audit_subject.clone()),
        quota_subject: mapping.and_then(|mapping| mapping.quota_subject.clone()),
        source_address: None,
    })
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut daemon = UnixStream::connect(socket)?;
    let envelope = IpcEnvelope {
        version: IPC_VERSION,
        requester: RequesterMetadata::try_from(&requester)?,
        session_id: session_id(),
        error: None,
    };
    IpcListener::send_envelope(&mut daemon, &envelope)?;

    let mut request = daemon.try_clone()?;
    std::thread::spawn(move || {
        let result = telchar::ipc::copy_bounded(io::stdin().lock(), &mut request);
        let _ = request.shutdown(std::net::Shutdown::Write);
        if let Err(error) = result {
            tracing::warn!(
                event = "ipc.frontend.request_relay_failed",
                reason = error_reason(&error),
                "frontend request relay failed"
            );
        }
    });
    telchar::ipc::copy_bounded(daemon, io::stdout().lock())?;
    Ok(())
}

fn daemon() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let telemetry = telemetry::Telemetry::initialize()?;
    let result = run_daemon();
    if let Err(error) = &result {
        tracing::error!(
            event = "ipc.daemon.connection_failed",
            reason = error_reason(error),
            "daemon connection failed"
        );
    }
    telemetry.shutdown();
    result.map_err(Into::into)
}

fn run_daemon() -> io::Result<()> {
    let config =
        telchar::config::ServiceConfig::load().map_err(|_| invalid("database migration failed"))?;
    let deployment = config.require_deployment()?.clone();
    let aggregate_features = config
        .backend_targets()
        .filter(|target| target.system() == deployment.system())
        .flat_map(|target| target.features().iter().cloned())
        .collect::<std::collections::BTreeSet<_>>();
    if aggregate_features
        != deployment
            .supported_features()
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    {
        return Err(invalid(
            "deployment supported features do not match configured backends",
        ));
    }
    let running_disconnect_policy = config.running_disconnect_policy();
    let transfer_limits = telchar::transfer_limits::TransferLimits::from_environment()?;
    let disk_reserve = telchar::disk_reserve::DiskReserve::from_environment()?;
    let database_url = config
        .require_database_url()
        .map_err(|_| invalid("database migration failed"))?
        .to_owned();
    tracing::info!(
        event = "database.migration.started",
        latest_migration_version = 12_i64,
        "database migration started"
    );
    let migration = match telchar::persistence::migrate(&database_url) {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::error!(
                event = "database.migration.failed",
                failure_class = error.failure().as_str(),
                "database migration failed"
            );
            return Err(invalid("database migration failed"));
        }
    };
    tracing::info!(
        event = "database.migration.completed",
        latest_migration_version = 12_i64,
        previously_applied_count = migration.previously_applied,
        applied_this_run_count = migration.applied_this_run,
        resulting_schema_version = migration.resulting_version,
        "database migration completed"
    );
    let mut singleton_ownership =
        match telchar::singleton_ownership::SingletonOwnership::acquire(&database_url) {
            Ok(ownership) => {
                tracing::info!(
                    event = "database.singleton_ownership.acquired",
                    operation = "acquire",
                    result = "success",
                    "singleton daemon ownership acquired"
                );
                ownership
            }
            Err(error) => {
                tracing::error!(
                    event = "database.singleton_ownership.refused",
                    operation = "acquire",
                    result = "failed",
                    failure_class = error.failure().as_str(),
                    "singleton daemon ownership refused"
                );
                return Err(invalid("singleton daemon ownership refused"));
            }
        };
    let mut store_retention = telchar::store_retention::backend_from_environment()?;
    telchar::store_retention::reconcile_output_retention(
        &database_url,
        store_retention.as_mut(),
        SystemTime::now(),
    )?;
    tracing::info!(
        event = "gateway.request_lease_release.completed",
        operation = "reconcile-release",
        owner_kind = "request",
        state = "released",
        result = "success",
        "released request roots reconciled"
    );
    let recovered_queued_requests =
        telchar::persistence::recover_queued_build_requests(&database_url, 256)
            .map_err(|_| invalid("queued request recovery failed"))?;
    tracing::info!(
        event = "database.build_request.recovered",
        operation = "recover-queued",
        request_state = "queued",
        recovered_count = recovered_queued_requests.len(),
        "queued requests recovered"
    );
    telchar::persistence::recover_dispatching_attempts(&database_url, 256)
        .map_err(|_| invalid("dispatching attempt recovery failed"))?;
    telchar::persistence::recover_backend_pending_attempts(&database_url, 256)
        .map_err(|_| invalid("backend-pending attempt recovery failed"))?;
    telchar::persistence::recover_running_attempts(&database_url, 256)
        .map_err(|_| invalid("running attempt recovery failed"))?;
    let collecting = telchar::persistence::recover_collecting_attempts(&database_url, 256)
        .map_err(|_| invalid("collecting attempt recovery failed"))?;
    recover_collecting_build_requests(&database_url, &deployment, collecting)?;
    let disk_probe = telchar::disk_reserve::OsDiskReserveProbe;
    telchar::static_ssh_backend::verify_configured_backends(
        config.static_ssh_backends(),
        Duration::from_secs(10),
    )?;
    let mut configured_backends = telchar::backend_routing::ConfiguredBackends::new(&config)?;
    let active_shared_builds = telchar::persistence::read_active_shared_builds(&database_url, 256)
        .map_err(|_| invalid("shared build recovery failed"))?;
    let reconciliation = if active_shared_builds.is_empty() {
        telchar::shared_build_recovery::ReconciliationOutcome::default()
    } else {
        let mut shared_build_outputs =
            telchar::shared_build_recovery::GatewaySharedBuildOutputStore::from_environment()?;
        telchar::shared_build_recovery::reconcile_shared_builds(
            &database_url,
            deployment.output_retention().duration(),
            active_shared_builds,
            &mut shared_build_outputs,
            &mut configured_backends,
        )?
    };
    tracing::info!(
        event = "database.shared_build.reconciled",
        succeeded_count = reconciliation.succeeded,
        failed_count = reconciliation.failed,
        monitoring_count = reconciliation.monitoring,
        "active shared builds reconciled"
    );
    let backends = Arc::new(configured_backends);
    let shared_builds = Arc::new(telchar::shared_build::SharedBuildRegistry::new());
    let scheduling_config = config.clone();
    let shared_build_scheduler = Arc::new(
        telchar::shared_build_scheduler::SharedBuildScheduler::new(
            database_url.clone(),
            move |quota_subject| scheduling_config.scheduling_limits(quota_subject),
        )
        .map_err(|_| invalid("shared build scheduler initialization failed"))?,
    );
    let object_admission = telchar::transfer_limits::ObjectAdmissionState::new(&transfer_limits);
    let rate_admission = telchar::transfer_limits::RateAdmissionState::new(&transfer_limits);
    tracing::info!(
        event = "deployment.configured",
        system = deployment.system(),
        supported_feature_count = deployment.supported_features().len(),
        running_disconnect_policy = running_disconnect_policy.as_str(),
        output_retention_seconds = deployment.output_retention().seconds(),
        "one-system deployment configured"
    );
    let socket = daemon_socket_argument()?;
    let expected_uid = daemon_uid_argument()?;
    prepare_socket_path(&socket)?;
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, Permissions::from_mode(0o600))?;
    let socket_guard = SocketGuard(socket);
    let listener = IpcListener::from_listener(listener, expected_uid);
    let envelope_timeout = duration_from_env("TELCHAR_IPC_ENVELOPE_TIMEOUT_MS", 5_000);
    let once = std::env::args().any(|argument| argument == "--once");
    if once {
        return serve_connection(
            &listener,
            envelope_timeout,
            &database_url,
            &deployment,
            &config,
            running_disconnect_policy,
            &transfer_limits,
            &object_admission,
            &rate_admission,
            disk_reserve,
            &disk_probe,
            &backends,
            &shared_builds,
            &shared_build_scheduler,
        );
    }
    let maintenance_database_url = database_url.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(60));
        let result =
            telchar::store_retention::backend_from_environment().and_then(|mut backend| {
                telchar::store_retention::reconcile_output_retention(
                    &maintenance_database_url,
                    backend.as_mut(),
                    SystemTime::now(),
                )
            });
        if result.is_err() {
            tracing::warn!(
                event = "gateway.output_retention.maintenance_failed",
                operation = "expire-output-retention",
                result = "failed",
            );
        }
    });
    let ownership_check_interval = duration_from_env("TELCHAR_SINGLETON_CHECK_INTERVAL_MS", 1_000);
    listener.set_nonblocking(true)?;
    let maximum_sessions = config.maximum_ipc_sessions();
    let active_sessions = Arc::new(Mutex::new(0_usize));
    let mut next_ownership_check = std::time::Instant::now() + ownership_check_interval;
    loop {
        if std::time::Instant::now() >= next_ownership_check {
            if let Err(error) = singleton_ownership.check() {
                tracing::error!(
                    event = "database.singleton_ownership.lost",
                    operation = "check",
                    result = "failed",
                    failure_class = error.failure().as_str(),
                    "singleton daemon ownership lost"
                );
                return Err(invalid("singleton daemon ownership lost"));
            }
            next_ownership_check = std::time::Instant::now() + ownership_check_interval;
        }
        let connection = match listener.accept_pending() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10).min(ownership_check_interval));
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    event = "ipc.daemon.connection_rejected",
                    reason = error_reason(&error),
                    "local IPC connection rejected"
                );
                continue;
            }
        };
        let permit = match SessionPermit::acquire(Arc::clone(&active_sessions), maximum_sessions) {
            Some(permit) => permit,
            None => {
                tracing::warn!(event = "ipc.daemon.session_rejected", reason = "capacity");
                drop(connection);
                continue;
            }
        };
        let database_url = database_url.clone();
        let deployment = deployment.clone();
        let service_config = config.clone();
        let object_admission = object_admission.clone();
        let rate_admission = rate_admission.clone();
        let backends = Arc::clone(&backends);
        let shared_builds = Arc::clone(&shared_builds);
        let shared_build_scheduler = Arc::clone(&shared_build_scheduler);
        std::thread::spawn(move || {
            let _permit = permit;
            let result = connection
                .receive_envelope(envelope_timeout)
                .and_then(|connection| {
                    serve_accepted_connection(
                        connection,
                        &database_url,
                        &deployment,
                        &service_config,
                        running_disconnect_policy,
                        &transfer_limits,
                        &object_admission,
                        &rate_admission,
                        disk_reserve,
                        &telchar::disk_reserve::OsDiskReserveProbe,
                        &backends,
                        &shared_builds,
                        &shared_build_scheduler,
                    )
                });
            if let Err(error) = result {
                tracing::warn!(
                    event = "ipc.daemon.session_failed",
                    reason = error_reason(&error),
                    "frontend session failed"
                );
            }
        });
        let _ = &socket_guard;
    }
}

#[allow(clippy::too_many_arguments)]
fn serve_connection(
    listener: &IpcListener,
    envelope_timeout: Duration,
    database_url: &str,
    deployment: &telchar::deployment::DeploymentConfig,
    service_config: &telchar::config::ServiceConfig,
    running_disconnect_policy: telchar::deployment::RunningDisconnectPolicy,
    transfer_limits: &telchar::transfer_limits::TransferLimits,
    object_admission: &telchar::transfer_limits::ObjectAdmissionState,
    rate_admission: &telchar::transfer_limits::RateAdmissionState,
    disk_reserve: telchar::disk_reserve::DiskReserve,
    disk_probe: &dyn telchar::disk_reserve::DiskReserveProbe,
    backends: &telchar::backend_routing::ConfiguredBackends,
    shared_builds: &telchar::shared_build::SharedBuildRegistry,
    shared_build_scheduler: &telchar::shared_build_scheduler::SharedBuildScheduler,
) -> io::Result<()> {
    serve_accepted_connection(
        listener.accept_with_envelope_timeout(envelope_timeout)?,
        database_url,
        deployment,
        service_config,
        running_disconnect_policy,
        transfer_limits,
        object_admission,
        rate_admission,
        disk_reserve,
        disk_probe,
        backends,
        shared_builds,
        shared_build_scheduler,
    )
}

#[allow(clippy::too_many_arguments)]
fn serve_accepted_connection(
    mut connection: telchar::ipc::IpcConnection,
    database_url: &str,
    deployment: &telchar::deployment::DeploymentConfig,
    service_config: &telchar::config::ServiceConfig,
    running_disconnect_policy: telchar::deployment::RunningDisconnectPolicy,
    transfer_limits: &telchar::transfer_limits::TransferLimits,
    object_admission: &telchar::transfer_limits::ObjectAdmissionState,
    rate_admission: &telchar::transfer_limits::RateAdmissionState,
    disk_reserve: telchar::disk_reserve::DiskReserve,
    disk_probe: &dyn telchar::disk_reserve::DiskReserveProbe,
    backends: &telchar::backend_routing::ConfiguredBackends,
    shared_builds: &telchar::shared_build::SharedBuildRegistry,
    shared_build_scheduler: &telchar::shared_build_scheduler::SharedBuildScheduler,
) -> io::Result<()> {
    if connection.envelope().error.is_some() {
        tracing::warn!(
            event = "ipc.daemon.session_rejected",
            reason = "frontend-error",
            "frontend session rejected"
        );
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frontend reported an IPC envelope error",
        ));
    }
    let session_id = connection.envelope().session_id.clone();
    let requester_reference =
        telchar::persistence::requester_reference(&connection.envelope().requester);
    if let Err(error) = telchar::persistence::open_protocol_session(
        database_url,
        &session_id,
        &requester_reference,
        &connection.envelope().requester.credential_id,
        &connection.envelope().requester.audit_subject,
        &connection.envelope().requester.quota_subject,
    ) {
        tracing::warn!(
            event = "database.protocol_session.failed",
            operation = "open",
            failure_class = error.failure().as_str(),
            "protocol session persistence failed"
        );
        return Err(invalid("protocol session state operation failed"));
    }
    tracing::info!(
        event = "database.protocol_session.opened",
        operation = "open",
        state = "open",
        "protocol session persisted"
    );
    tracing::info!(
        event = "ipc.daemon.session_started",
        "authenticated frontend session started"
    );
    let result = (|| {
        let input = connection.stream_mut().try_clone()?;
        let mut store_query = telchar::store_query::GatewayStoreQuery::from_environment();
        let mut build_executor = backends.executor();
        let mut store_export = telchar::store_export::backend_from_environment()?;
        let mut store_import = telchar::store_import::importer_from_environment()?;
        let mut store_closure = telchar::store_closure::backend_from_environment()?;
        let mut store_retention = telchar::store_retention::backend_from_environment()?;
        telchar::session::run_worker_session(
            input,
            connection.stream_mut().try_clone()?,
            protocol_session_limits(),
            deployment,
            running_disconnect_policy,
            &mut store_query,
            &mut build_executor,
            store_export.as_mut(),
            store_import.as_mut(),
            store_closure.as_mut(),
            store_retention.as_mut(),
            database_url,
            &session_id,
            &connection.envelope().requester.audit_subject,
            &connection.envelope().requester.quota_subject,
            transfer_limits,
            object_admission,
            rate_admission,
            disk_reserve,
            disk_probe,
            shared_builds,
            shared_build_scheduler,
            service_config.scheduling_limits(&connection.envelope().requester.quota_subject),
        )
    })();
    match telchar::persistence::close_protocol_session(database_url, &session_id) {
        Ok(_) => tracing::info!(
            event = "database.protocol_session.closed",
            operation = "close",
            state = "closed",
            "protocol session persisted"
        ),
        Err(error) => {
            tracing::warn!(
                event = "database.protocol_session.failed",
                operation = "close",
                failure_class = error.failure().as_str(),
                "protocol session persistence failed"
            );
            if result.is_ok() {
                return Err(invalid("protocol session state operation failed"));
            }
        }
    }
    result
}

fn prepare_socket_path(socket: &std::path::Path) -> io::Result<()> {
    let parent = socket
        .parent()
        .ok_or_else(|| invalid("daemon socket requires a parent directory"))?;
    match std::fs::create_dir(parent) {
        Ok(()) => std::fs::set_permissions(parent, Permissions::from_mode(0o700))?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(parent)?;
            if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "daemon runtime directory is not private",
                ));
            }
        }
        Err(error) => return Err(error),
    }
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) if metadata.file_type().is_socket() => match UnixStream::connect(socket) {
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "daemon socket is already accepting connections",
            )),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                std::fs::remove_file(socket)
            }
            Err(error) => Err(error),
        },
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "daemon socket path exists and is not a socket",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

struct SessionPermit(Arc<Mutex<usize>>);

impl SessionPermit {
    fn acquire(active: Arc<Mutex<usize>>, maximum: usize) -> Option<Self> {
        let mut count = active.lock().expect("session count mutex is not poisoned");
        if *count >= maximum {
            return None;
        }
        *count += 1;
        drop(count);
        Some(Self(active))
    }
}

impl Drop for SessionPermit {
    fn drop(&mut self) {
        let mut count = self.0.lock().expect("session count mutex is not poisoned");
        *count -= 1;
    }
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn protocol_session_limits() -> nix_worker_protocol::ProtocolSessionLimits {
    let default = nix_worker_protocol::ProtocolSessionLimits::DEFAULT;
    nix_worker_protocol::ProtocolSessionLimits::new(
        default.maximum_retained_metadata_bytes,
        duration_from_env(
            "TELCHAR_WORKER_IDLE_TIMEOUT_MS",
            default.incomplete_message_idle_timeout.as_millis() as u64,
        ),
    )
}

fn daemon_socket_argument() -> io::Result<PathBuf> {
    let mut arguments = std::env::args().skip(2);
    while let Some(argument) = arguments.next() {
        if argument == "--socket" {
            return arguments
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| invalid("--socket requires a path"));
        }
    }
    Err(invalid("daemon requires --socket"))
}

fn daemon_uid_argument() -> io::Result<u32> {
    let mut arguments = std::env::args().skip(2);
    while let Some(argument) = arguments.next() {
        if argument == "--frontend-uid" {
            return arguments
                .next()
                .ok_or_else(|| invalid("--frontend-uid requires a value"))?
                .parse()
                .map_err(|_| invalid("--frontend-uid must be an unsigned integer"));
        }
    }
    Err(invalid("daemon requires --frontend-uid"))
}

fn required_path(name: &'static str) -> io::Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("required frontend environment is absent"))
}

fn required_string(name: &'static str) -> io::Result<String> {
    std::env::var(name).map_err(|_| {
        let _ = name;
        invalid("required frontend environment is absent")
    })
}

fn session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn duration_from_env(name: &str, default_ms: u64) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(default_ms))
}

fn u32_from_env(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn error_reason(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "permission-denied",
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "timeout",
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => "invalid-input",
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => "unavailable",
        _ => "io-error",
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
