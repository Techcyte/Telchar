//! Owns Telchar command execution, daemon composition, IPC serving, and shutdown.

use std::fs::Permissions;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::telemetry;
use telchar::service::identity::{IdentityInput, normalize_requester};
use telchar::service::ipc::{IPC_VERSION, IpcEnvelope, IpcListener, RequesterMetadata};

#[path = "runtime/daemon.rs"]
mod daemon_runtime;

use daemon_runtime::{
    SessionPermit, SocketGuard, prepare_socket_path, serve_accepted_connection, serve_connection,
    shutdown_daemon_services,
};

pub(crate) fn validate_database_tls() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let database_url_file = std::env::args_os()
        .nth(2)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("database TLS validation requires a URL file"))?;
    let root_certificate = std::env::args_os()
        .nth(3)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("database TLS validation requires a root certificate"))?;
    let database_url = std::fs::read_to_string(database_url_file)
        .map_err(|_| invalid("database URL file could not be read"))?;
    telchar::persistence::validate_verified_connection(database_url.trim(), &root_certificate)
        .map_err(|_| invalid("database URL must use effective sslmode=verify-full and the configured root certificate"))?;
    Ok(())
}

pub(crate) fn executor() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let telemetry = telemetry::Telemetry::initialize()?;
    let result = run_executor();
    telemetry.shutdown();
    result.map_err(Into::into)
}

fn run_executor() -> io::Result<()> {
    let config = telchar::service::config::ServiceConfig::load()
        .map_err(|error| configuration_failure("load-or-validation", error))?;
    let database_url = config
        .require_database_url()
        .map_err(|error| configuration_failure("database-url-missing", error))?
        .to_owned();
    telchar::persistence::migrate(&database_url)
        .map_err(|_| invalid("database migration failed"))?;
    let mut ownership =
        telchar::service::singleton_ownership::SingletonOwnership::acquire_local_executor(
            &database_url,
            config.ownership_lease_duration(),
        )
        .map_err(|_| invalid("local executor ownership refused"))?;
    let database = telchar::persistence::Database::connect(ownership.database_url())
        .map_err(|_| invalid("database connection failed"))?;
    let ownership_renewal_interval = config.ownership_renewal_interval();
    let socket = required_path("TELCHAR_EXECUTOR_SOCKET")?;
    let expected_uid = u32_from_env("TELCHAR_EXECUTOR_UID", rustix::process::getuid().as_raw())?;
    prepare_socket_path(&socket)?;
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, Permissions::from_mode(0o600))?;
    let _socket_guard = SocketGuard(socket);
    listener.set_nonblocking(true)?;
    let executor = Arc::new(Mutex::new(
        telchar::backend::local::executor_from_environment()?,
    ));
    let mut next_ownership_renewal = std::time::Instant::now() + ownership_renewal_interval;
    loop {
        if std::time::Instant::now() >= next_ownership_renewal {
            ownership
                .renew()
                .map_err(|_| invalid("local executor ownership lost"))?;
            next_ownership_renewal = std::time::Instant::now() + ownership_renewal_interval;
        }
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10).min(ownership_renewal_interval));
                continue;
            }
            Err(error) => return Err(error),
        };
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
                let database = database.clone();
                let specification = specification.clone();
                let executor = Arc::clone(&executor);
                std::thread::spawn(move || {
                    if telchar::persistence::record_local_backend_running(
                        &database,
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
                            &database,
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
            &database,
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
    let frontend = tracing::info_span!("ipc.frontend");
    let _entered = frontend.enter();
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

#[tracing::instrument(level = "trace", skip_all)]
fn run_frontend() -> io::Result<()> {
    let config = telchar::service::config::ServiceConfig::load()?;
    let socket = config.require_ipc_socket()?;
    let identity = if std::env::var_os("TELCHAR_AUTHENTICATED_CA").is_some() {
        if std::env::var_os("TELCHAR_AUTHENTICATED_KEY").is_some() {
            return Err(invalid("authenticated identity is ambiguous"));
        }
        IdentityInput::Certificate {
            ca_fingerprint: required_string("TELCHAR_AUTHENTICATED_CA")?,
            key_id: required_string("TELCHAR_AUTHENTICATED_KEY_ID")?,
            principals: required_string("TELCHAR_AUTHENTICATED_PRINCIPALS")?
                .lines()
                .map(str::to_owned)
                .collect(),
            audit_subject: None,
            quota_subject: None,
            source_address: None,
        }
    } else {
        IdentityInput::PublicKey {
            fingerprint: required_string("TELCHAR_AUTHENTICATED_KEY")?,
            audit_subject: None,
            quota_subject: None,
            source_address: None,
        }
    };
    let mut requester = normalize_requester(identity)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if let Some(mapping) = config.credential_mapping(&requester.credential_id) {
        if let Some(subject) = &mapping.audit_subject {
            requester.audit_subject = subject.clone();
        }
        if let Some(subject) = &mapping.quota_subject {
            requester.quota_subject = subject.clone();
        }
    }
    let connecting = tracing::enabled!(tracing::Level::TRACE).then(std::time::Instant::now);
    let daemon = UnixStream::connect(socket);
    tracing::trace!(
        event = "ipc.frontend.connected",
        elapsed_us = connecting.map(|start| start.elapsed().as_micros() as u64),
        success = daemon.is_ok()
    );
    let mut daemon = daemon?;
    let envelope = IpcEnvelope {
        version: IPC_VERSION,
        requester: RequesterMetadata::try_from(&requester)?,
        session_id: session_id(),
        error: None,
    };
    let _session =
        tracing::trace_span!("ipc.frontend.session", session_id = %envelope.session_id).entered();
    IpcListener::send_envelope(&mut daemon, &envelope)?;
    tracing::trace!(event = "ipc.frontend.envelope_sent");

    let mut request = daemon.try_clone()?;
    let parent = tracing::Span::current();
    std::thread::spawn(move || {
        let _parent = parent.enter();
        let _relay = tracing::trace_span!("ipc.frontend.relay", direction = "request").entered();
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
    let _relay = tracing::trace_span!("ipc.frontend.relay", direction = "response").entered();
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

fn configuration_failure(failure_class: &'static str, error: io::Error) -> io::Error {
    tracing::error!(
        event = "configuration.failed",
        failure_class,
        diagnostic = %error,
        "service configuration failed"
    );
    error
}

fn run_daemon() -> io::Result<()> {
    let mut config = telchar::service::config::ServiceConfig::load()
        .map_err(|error| configuration_failure("load-or-validation", error))?;
    let running_disconnect_policy = config.running_disconnect_policy();
    let output_retention = config.output_retention();
    let maximum_retained_input_bytes = config.maximum_retained_input_bytes();
    let transfer_limits = telchar::service::transfer_limits::TransferLimits::from_environment()?;
    let disk_reserve = telchar::service::disk_reserve::DiskReserve::from_environment()?;
    telchar::service::disk_reserve::gateway_store_directory()
        .map_err(|error| configuration_failure("gateway-store-directory", error))?;
    let gateway_store = telchar::store::runtime::GatewayStoreRuntime::from_environment()?;
    let database_url = config
        .require_database_url()
        .map_err(|error| configuration_failure("database-url-missing", error))?
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
        match telchar::service::singleton_ownership::SingletonOwnership::acquire(
            &database_url,
            config.ownership_lease_duration(),
        ) {
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
    let database = telchar::persistence::Database::connect(singleton_ownership.database_url())
        .map_err(|_| invalid("database connection failed"))?;
    let mut store_retention = gateway_store.retention()?;
    singleton_ownership
        .maintain_during(config.ownership_renewal_interval(), || {
            telchar::store::retention::reconcile_output_retention(
                &database,
                store_retention.as_mut(),
                SystemTime::now(),
            )
        })
        .map_err(|_| invalid("singleton daemon ownership lost"))??;
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
    let active_shared_builds = telchar::persistence::read_active_shared_builds(&database, 256)
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
            &database,
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
    let operational_counts = telchar::persistence::read_shared_build_operational_counts(&database)
        .map_err(|_| invalid("shared build metric reconciliation failed"))?;
    telchar::service::metrics::record_shared_build_operational_counts(operational_counts);
    let monitoring_derivations = reconciliation.monitoring_derivations;
    let backends = telchar::backend::routing::ReloadableBackends::new(configured_backends);
    let shared_builds = Arc::new(telchar::shared_build::SharedBuildRegistry::new());
    let scheduling_config = config.clone();
    let shared_build_scheduler = Arc::new(
        telchar::shared_build::scheduler::SharedBuildScheduler::new(
            database.clone(),
            move |quota_subject| scheduling_config.scheduling_limits(quota_subject),
        )
        .map_err(|_| invalid("shared build scheduler initialization failed"))?,
    );
    let mut recovery_services = Vec::with_capacity(monitoring_derivations.len());
    for derivation_path in monitoring_derivations {
        let database = database.clone();
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
                        &database,
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
    let mut static_ssh_consul_service = if config.static_ssh_consul().is_empty() {
        None
    } else {
        Some(
            telchar::service::static_ssh_consul::ConsulSshDiscoveryService::start(
                config.static_ssh_consul().to_vec(),
                config
                    .static_ssh_consul()
                    .iter()
                    .map(|source| source.refresh_interval())
                    .min()
                    .ok_or_else(|| invalid("Consul SSH discovery configuration is invalid"))?,
            )?,
        )
    };
    let mut callback_service = if let Some(callback) = config.nomad_callback() {
        let callback_listener = std::net::TcpListener::bind(callback.bind())?;
        Some(
            telchar::nomad::callback_service::NomadCallbackService::start(
                callback_listener,
                callback.clone(),
                database.clone(),
                config.nomad_backends().to_vec(),
                gateway_store
                    .endpoint()
                    .cloned()
                    .ok_or_else(|| invalid("gateway store endpoint is not configured"))?,
                output_retention.duration(),
                Arc::clone(&shared_builds),
            )?,
        )
    } else {
        None
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
            &database,
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
    let maintenance_database = database.clone();
    let maintenance_gateway_store = gateway_store.clone();
    let mut maintenance_service = telchar::service::daemon_services::MaintenanceService::start(
        Duration::from_secs(60),
        move || {
            let mut backend = maintenance_gateway_store.retention()?;
            telchar::store::retention::reconcile_output_retention(
                &maintenance_database,
                backend.as_mut(),
                SystemTime::now(),
            )
        },
    )?;
    let ownership_check_interval = config.ownership_renewal_interval();
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
    let mut discovered_static_ssh = Vec::new();
    let mut next_ownership_check = std::time::Instant::now() + ownership_check_interval;
    loop {
        if shutdown_requested.load(std::sync::atomic::Ordering::Relaxed) {
            shutdown_daemon_services(
                &mut callback_service,
                &mut maintenance_service,
                &mut static_ssh_health_service,
                &mut static_ssh_consul_service,
                &mut recovery_services,
            )?;
            return Ok(());
        }
        if reload_requested.swap(false, std::sync::atomic::Ordering::Relaxed) {
            let reload_started = std::time::Instant::now();
            tracing::info!(
                event = "configuration.reload.requested",
                "configuration reload requested"
            );
            match telchar::service::config_reload::BackendReload::prepare(
                &config,
                &discovered_static_ssh,
                gateway_store.endpoint().cloned(),
                gateway_store
                    .build_helper()
                    .map(std::path::Path::to_path_buf),
                Duration::from_secs(1),
            )
            .and_then(|reload| reload.apply(&mut config, &backends, &mut static_ssh_health_service))
            {
                Ok(changes) => {
                    telchar::service::metrics::configuration_reload(
                        reload_started.elapsed(),
                        "succeeded",
                        None,
                        Some(changes),
                    );
                    tracing::info!(
                        event = "configuration.reload.completed",
                        static_ssh_added_count = changes.added,
                        static_ssh_removed_count = changes.removed,
                        static_ssh_total_count = config.static_ssh_backends().len(),
                        "configuration reload completed"
                    );
                }
                Err(error) => {
                    telchar::service::metrics::configuration_reload(
                        reload_started.elapsed(),
                        "rejected",
                        Some("invalid"),
                        None,
                    );
                    tracing::warn!(
                        event = "configuration.reload.rejected",
                        reason = error_reason(&error),
                        "configuration reload rejected"
                    );
                }
            }
        }
        if let Some(service) = static_ssh_consul_service.as_mut()
            && let Some(discovered) = service.check()?
        {
            telchar::service::static_ssh_consul::publish_inventory(
                &config,
                &discovered,
                &backends,
                gateway_store.endpoint().cloned(),
                gateway_store
                    .build_helper()
                    .map(std::path::Path::to_path_buf),
            )?;
            discovered_static_ssh = discovered;
        }
        if let Err(error) = static_ssh_health_service.check() {
            shutdown_daemon_services(
                &mut callback_service,
                &mut maintenance_service,
                &mut static_ssh_health_service,
                &mut static_ssh_consul_service,
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
                &mut static_ssh_consul_service,
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
                        &mut static_ssh_consul_service,
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
                    &mut static_ssh_consul_service,
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
                    detail = %error,
                    "local IPC connection rejected"
                );
                continue;
            }
        };
        let permit = match SessionPermit::acquire(Arc::clone(&active_sessions), maximum_sessions) {
            Some(permit) => permit,
            None => {
                telchar::service::metrics::session_rejected("capacity");
                tracing::warn!(event = "ipc.daemon.session_rejected", reason = "capacity");
                drop(connection);
                continue;
            }
        };
        let database = database.clone();
        let service_config = config.clone();
        let object_admission = object_admission.clone();
        let rate_admission = rate_admission.clone();
        let backends = backends.clone();
        let shared_builds = Arc::clone(&shared_builds);
        let shared_build_scheduler = Arc::clone(&shared_build_scheduler);
        let gateway_store = gateway_store.clone();
        std::thread::spawn(move || {
            let _permit = permit;
            let mut accepted_session_id = None;
            let result = connection
                .receive_envelope(envelope_timeout)
                .and_then(|connection| {
                    let session_id = connection.envelope().session_id.clone();
                    accepted_session_id = Some(session_id.clone());
                    serve_accepted_connection(
                        connection,
                        &database,
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
                    session_id = accepted_session_id.as_deref().unwrap_or("unavailable"),
                    reason = error_reason(&error),
                    diagnostic = %error,
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

fn u32_from_env(name: &str, default: u32) -> io::Result<u32> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|_| invalid("numeric environment override is invalid")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(invalid("numeric environment override is invalid"))
        }
    }
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
