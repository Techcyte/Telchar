//! Owns Telchar command execution, daemon composition, IPC serving, and shutdown.

use std::fs::Permissions;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::telemetry;
use telchar::service::identity::{normalize_requester, IdentityInput};
use telchar::service::ipc::{IpcEnvelope, IpcListener, RequesterMetadata, IPC_VERSION};

#[path = "runtime/daemon.rs"]
mod daemon_runtime;

use daemon_runtime::{
    prepare_socket_path, serve_accepted_connection, serve_connection, shutdown_daemon_services,
    SessionPermit, SocketGuard,
};

pub(crate) fn executor() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let telemetry = telemetry::Telemetry::initialize()?;
    let result = run_executor();
    telemetry.shutdown();
    result.map_err(Into::into)
}

fn run_executor() -> io::Result<()> {
    let config = telchar::service::config::ServiceConfig::load()
        .map_err(|_| invalid("database migration failed"))?;
    let database_url = config
        .require_database_url()
        .map_err(|_| invalid("database migration failed"))?
        .to_owned();
    telchar::persistence::migrate(&database_url)
        .map_err(|_| invalid("database migration failed"))?;
    let _ownership =
        telchar::service::singleton_ownership::SingletonOwnership::acquire_local_executor(
            &database_url,
        )
        .map_err(|_| invalid("local executor ownership refused"))?;
    let socket = required_path("TELCHAR_EXECUTOR_SOCKET")?;
    let expected_uid = u32_from_env("TELCHAR_EXECUTOR_UID", rustix::process::getuid().as_raw());
    prepare_socket_path(&socket)?;
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, Permissions::from_mode(0o600))?;
    let _socket_guard = SocketGuard(socket);
    let executor = Arc::new(Mutex::new(
        telchar::backend::local::executor_from_environment()?,
    ));
    for connection in listener.incoming() {
        let mut stream = connection?;
        if telchar::service::ipc::authorize_peer(&stream, expected_uid).is_err() {
            continue;
        }
        let mut submit =
            |backend_execution_id: &str,
             specification: &telchar::service::executor_service::ExecutorSpecification,
             execution: &telchar::persistence::LocalBackendExecution| {
                specification.build.validate_for_execution()?;
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
        if let Err(error) = telchar::service::executor_service::handle_connection_with_submit(
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

pub(crate) fn smoke() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
        if std::env::var_os("TELCHAR_SMOKE_OPERATIONAL_METRICS").is_some() {
            telchar::service::metrics::emit_smoke_metrics();
        }
        if std::env::var_os("TELCHAR_SMOKE_ERROR").is_some() {
            tracing::error!(event = "smoke.error", request_id = %request_id, "smoke error");
        }
    }
    println!("{}", nix_worker_protocol::protocol_name());

    telemetry.shutdown();
    Ok(())
}

pub(crate) fn serve_stdio() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    let config = telchar::service::config::ServiceConfig::load()?;
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
        let result = telchar::service::ipc::copy_bounded(io::stdin().lock(), &mut request);
        let _ = request.shutdown(std::net::Shutdown::Write);
        if let Err(error) = result {
            tracing::warn!(
                event = "ipc.frontend.request_relay_failed",
                reason = error_reason(&error),
                "frontend request relay failed"
            );
        }
    });
    telchar::service::ipc::copy_bounded(daemon, io::stdout().lock())?;
    Ok(())
}

pub(crate) fn daemon() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    let mut config = telchar::service::config::ServiceConfig::load()
        .map_err(|_| invalid("database migration failed"))?;
    let running_disconnect_policy = config.running_disconnect_policy();
    let output_retention = config.output_retention();
    let maximum_retained_input_bytes = config.maximum_retained_input_bytes();
    let transfer_limits = telchar::service::transfer_limits::TransferLimits::from_environment()?;
    let disk_reserve = telchar::service::disk_reserve::DiskReserve::from_environment()?;
    let gateway_store = telchar::store::runtime::GatewayStoreRuntime::from_environment()?;
    let database_url = config
        .require_database_url()
        .map_err(|_| invalid("database migration failed"))?
        .to_owned();
    tracing::info!(
        event = "database.migration.started",
        latest_migration_version = telchar::persistence::latest_migration_version(),
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
        latest_migration_version = telchar::persistence::latest_migration_version(),
        previously_applied_count = migration.previously_applied,
        applied_this_run_count = migration.applied_this_run,
        resulting_schema_version = migration.resulting_version,
        "database migration completed"
    );
    let mut singleton_ownership =
        match telchar::service::singleton_ownership::SingletonOwnership::acquire(&database_url) {
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
    let mut store_retention = gateway_store.retention()?;
    telchar::store::retention::reconcile_output_retention(
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
    let disk_probe = telchar::service::disk_reserve::OsDiskReserveProbe;
    let static_ssh_health =
        telchar::backend::static_ssh::StaticSshHealth::probe_all(config.static_ssh_backends());
    let mut configured_backends = telchar::backend::routing::ConfiguredBackends::with_health(
        &config,
        gateway_store.endpoint().cloned(),
        gateway_store
            .build_helper()
            .map(std::path::Path::to_path_buf),
        static_ssh_health.clone(),
    )?;
    let active_shared_builds = telchar::persistence::read_active_shared_builds(&database_url, 256)
        .map_err(|_| invalid("shared build recovery failed"))?;
    let recovery_started = std::time::Instant::now();
    telchar::service::metrics::recovery_started("startup");
    let reconciliation_result = if active_shared_builds.is_empty() {
        Ok(telchar::shared_build::recovery::ReconciliationOutcome::default())
    } else {
        let mut shared_build_outputs =
            telchar::shared_build::recovery::GatewaySharedBuildOutputStore::with_endpoint(
                gateway_store.endpoint().cloned(),
            );
        telchar::shared_build::recovery::reconcile_shared_builds(
            &database_url,
            output_retention.duration(),
            active_shared_builds,
            &mut shared_build_outputs,
            &mut configured_backends,
        )
    };
    let reconciliation = match reconciliation_result {
        Ok(outcome) => {
            telchar::service::metrics::recovery_finished(
                "startup",
                recovery_started.elapsed(),
                outcome.succeeded,
                outcome.failed,
                outcome.monitoring,
            );
            outcome
        }
        Err(error) => {
            telchar::service::metrics::recovery_failed(
                "startup",
                recovery_started.elapsed(),
                telchar::service::metrics::io_failure_class(&error),
            );
            return Err(error);
        }
    };
    tracing::info!(
        event = "database.shared_build.reconciled",
        succeeded_count = reconciliation.succeeded,
        failed_count = reconciliation.failed,
        monitoring_count = reconciliation.monitoring,
        "active shared builds reconciled"
    );
    let operational_counts =
        telchar::persistence::read_shared_build_operational_counts(&database_url)
            .map_err(|_| invalid("shared build metric reconciliation failed"))?;
    telchar::service::metrics::record_shared_build_operational_counts(operational_counts);
    let monitoring_derivations = reconciliation.monitoring_derivations;
    let backends = telchar::backend::routing::ReloadableBackends::new(configured_backends);
    let shared_builds = Arc::new(telchar::shared_build::SharedBuildRegistry::new());
    let scheduling_config = config.clone();
    let shared_build_scheduler = Arc::new(
        telchar::shared_build::scheduler::SharedBuildScheduler::new(
            database_url.clone(),
            move |quota_subject| scheduling_config.scheduling_limits(quota_subject),
        )
        .map_err(|_| invalid("shared build scheduler initialization failed"))?,
    );
    let mut recovery_services = Vec::with_capacity(monitoring_derivations.len());
    for derivation_path in monitoring_derivations {
        let database_url = database_url.clone();
        let backends = backends.clone();
        let retention = output_retention.duration();
        let gateway_store = gateway_store.endpoint().cloned();
        recovery_services.push(
            telchar::service::daemon_services::RecoveryMonitorService::start(
                Duration::from_millis(100),
                move || {
                    let mut configured_backends = backends.snapshot();
                    let mut outputs =
                        telchar::shared_build::recovery::GatewaySharedBuildOutputStore::with_endpoint(
                            gateway_store.clone(),
                        );
                    let outcome = telchar::shared_build::recovery::reconcile_adopted_shared_builds(
                        &database_url,
                        retention,
                        std::slice::from_ref(&derivation_path),
                        &mut outputs,
                        &mut configured_backends,
                    )?;
                    Ok(outcome.monitoring == 1)
                },
            )
            .map_err(|_| invalid("shared build recovery monitor failed"))?,
        );
    }
    let mut static_ssh_health_service =
        telchar::service::daemon_services::StaticSshHealthService::start(
            static_ssh_health,
            Duration::from_secs(1),
        )?;
    let mut callback_service = if config.nomad_backends().is_empty() {
        None
    } else {
        let callback_listener = std::net::TcpListener::bind(config.nomad_callback().bind())?;
        Some(
            telchar::nomad::callback_service::NomadCallbackService::start(
                callback_listener,
                config.nomad_callback().clone(),
                database_url.clone(),
                config.nomad_backends().to_vec(),
                gateway_store
                    .endpoint()
                    .cloned()
                    .ok_or_else(|| invalid("gateway store endpoint is not configured"))?,
                output_retention.duration(),
            )?,
        )
    };
    let object_admission =
        telchar::service::transfer_limits::ObjectAdmissionState::new(&transfer_limits);
    let rate_admission =
        telchar::service::transfer_limits::RateAdmissionState::new(&transfer_limits);
    tracing::info!(
        event = "backend.fleet.configured",
        backend_count = config.backend_targets().count(),
        system_count = config.system_features().len(),
        running_disconnect_policy = running_disconnect_policy.as_str(),
        output_retention_seconds = output_retention.seconds(),
        "multi-system backend fleet configured"
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
        let result = serve_connection(
            &listener,
            envelope_timeout,
            &database_url,
            &config,
            running_disconnect_policy,
            output_retention,
            maximum_retained_input_bytes,
            &transfer_limits,
            &object_admission,
            &rate_admission,
            disk_reserve,
            &disk_probe,
            &backends.snapshot(),
            &shared_builds,
            &shared_build_scheduler,
            &gateway_store,
        );
        if let Some(service) = callback_service.as_mut() {
            service.shutdown()?;
        }
        for service in &mut recovery_services {
            service.shutdown()?;
        }
        return result;
    }
    let maintenance_database_url = database_url.clone();
    let maintenance_gateway_store = gateway_store.clone();
    let mut maintenance_service = telchar::service::daemon_services::MaintenanceService::start(
        Duration::from_secs(60),
        move || {
            let mut backend = maintenance_gateway_store.retention()?;
            telchar::store::retention::reconcile_output_retention(
                &maintenance_database_url,
                backend.as_mut(),
                SystemTime::now(),
            )
        },
    )?;
    let ownership_check_interval = duration_from_env("TELCHAR_SINGLETON_CHECK_INTERVAL_MS", 1_000);
    let shutdown_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reload_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
    signal_hook::flag::register(
        signal_hook::consts::SIGTERM,
        Arc::clone(&shutdown_requested),
    )?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&shutdown_requested))?;
    signal_hook::flag::register(signal_hook::consts::SIGHUP, Arc::clone(&reload_requested))?;
    listener.set_nonblocking(true)?;
    let maximum_sessions = config.maximum_ipc_sessions();
    telchar::service::metrics::record_service_session_limit(maximum_sessions as u64);
    let active_sessions = Arc::new(Mutex::new(0_usize));
    let mut next_ownership_check = std::time::Instant::now() + ownership_check_interval;
    loop {
        if shutdown_requested.load(std::sync::atomic::Ordering::Relaxed) {
            shutdown_daemon_services(
                &mut callback_service,
                &mut maintenance_service,
                &mut static_ssh_health_service,
                &mut recovery_services,
            )?;
            return Ok(());
        }
        if reload_requested.swap(false, std::sync::atomic::Ordering::Relaxed) {
            tracing::info!(
                event = "configuration.reload.requested",
                "configuration reload requested"
            );
            match telchar::service::config_reload::BackendReload::prepare(
                &config,
                gateway_store.endpoint().cloned(),
                gateway_store
                    .build_helper()
                    .map(std::path::Path::to_path_buf),
                Duration::from_secs(1),
            )
            .and_then(|reload| reload.apply(&mut config, &backends, &mut static_ssh_health_service))
            {
                Ok(changes) => {
                    telchar::service::metrics::configuration_reload("succeeded", None);
                    tracing::info!(
                        event = "configuration.reload.completed",
                        static_ssh_added_count = changes.added,
                        static_ssh_removed_count = changes.removed,
                        static_ssh_total_count = config.static_ssh_backends().len(),
                        "configuration reload completed"
                    );
                }
                Err(error) => {
                    telchar::service::metrics::configuration_reload("rejected", Some("invalid"));
                    tracing::warn!(
                        event = "configuration.reload.rejected",
                        reason = error_reason(&error),
                        "configuration reload rejected"
                    );
                }
            }
        }
        if let Err(error) = static_ssh_health_service.check() {
            shutdown_daemon_services(
                &mut callback_service,
                &mut maintenance_service,
                &mut static_ssh_health_service,
                &mut recovery_services,
            )?;
            return Err(error);
        }
        if let Err(error) = maintenance_service.check() {
            tracing::error!(
                event = "gateway.output_retention.maintenance_failed",
                operation = "expire-output-retention",
                result = "failed",
                "output retention maintenance failed"
            );
            shutdown_daemon_services(
                &mut callback_service,
                &mut maintenance_service,
                &mut static_ssh_health_service,
                &mut recovery_services,
            )?;
            return Err(error);
        }
        let mut recovery_index = 0;
        while recovery_index < recovery_services.len() {
            match recovery_services[recovery_index].check() {
                Ok(true) => recovery_index += 1,
                Ok(false) => {
                    let mut service = recovery_services.remove(recovery_index);
                    service.shutdown()?;
                }
                Err(error) => {
                    tracing::error!(
                        event = "database.shared_build.recovery_monitor_failed",
                        result = "failed",
                        "shared build recovery monitor failed"
                    );
                    shutdown_daemon_services(
                        &mut callback_service,
                        &mut maintenance_service,
                        &mut static_ssh_health_service,
                        &mut recovery_services,
                    )?;
                    return Err(error);
                }
            }
        }
        if std::time::Instant::now() >= next_ownership_check {
            if let Err(error) = singleton_ownership.check() {
                tracing::error!(
                    event = "database.singleton_ownership.lost",
                    operation = "check",
                    result = "failed",
                    failure_class = error.failure().as_str(),
                    "singleton daemon ownership lost"
                );
                shutdown_daemon_services(
                    &mut callback_service,
                    &mut maintenance_service,
                    &mut static_ssh_health_service,
                    &mut recovery_services,
                )?;
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
        let service_config = config.clone();
        let object_admission = object_admission.clone();
        let rate_admission = rate_admission.clone();
        let backends = backends.clone();
        let shared_builds = Arc::clone(&shared_builds);
        let shared_build_scheduler = Arc::clone(&shared_build_scheduler);
        let gateway_store = gateway_store.clone();
        std::thread::spawn(move || {
            let _permit = permit;
            let result = connection
                .receive_envelope(envelope_timeout)
                .and_then(|connection| {
                    serve_accepted_connection(
                        connection,
                        &database_url,
                        &service_config,
                        running_disconnect_policy,
                        output_retention,
                        maximum_retained_input_bytes,
                        &transfer_limits,
                        &object_admission,
                        &rate_admission,
                        disk_reserve,
                        &telchar::service::disk_reserve::OsDiskReserveProbe,
                        &backends.snapshot(),
                        &shared_builds,
                        &shared_build_scheduler,
                        &gateway_store,
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
