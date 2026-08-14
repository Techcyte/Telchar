//! Serves authenticated daemon IPC connections and manages the daemon socket lifecycle.

use std::fs::Permissions;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use telchar::service::ipc::IpcListener;

use super::{invalid, protocol_session_limits};

pub(super) fn shutdown_daemon_services(
    callback: &mut Option<telchar::nomad::callback_service::NomadCallbackService>,
    maintenance: &mut telchar::service::daemon_services::MaintenanceService,
    recovery: &mut [telchar::service::daemon_services::RecoveryMonitorService],
) -> io::Result<()> {
    if let Some(service) = callback.as_mut() {
        service.shutdown()?;
    }
    maintenance.shutdown()?;
    for service in recovery {
        service.shutdown()?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn serve_connection(
    listener: &IpcListener,
    envelope_timeout: Duration,
    database_url: &str,
    service_config: &telchar::service::config::ServiceConfig,
    running_disconnect_policy: telchar::service::deployment::RunningDisconnectPolicy,
    output_retention: telchar::service::deployment::OutputRetention,
    maximum_retained_input_bytes: u64,
    transfer_limits: &telchar::service::transfer_limits::TransferLimits,
    object_admission: &telchar::service::transfer_limits::ObjectAdmissionState,
    rate_admission: &telchar::service::transfer_limits::RateAdmissionState,
    disk_reserve: telchar::service::disk_reserve::DiskReserve,
    disk_probe: &dyn telchar::service::disk_reserve::DiskReserveProbe,
    backends: &telchar::backend::routing::ConfiguredBackends,
    shared_builds: &telchar::shared_build::SharedBuildRegistry,
    shared_build_scheduler: &telchar::shared_build::scheduler::SharedBuildScheduler,
    gateway_store: &telchar::store::runtime::GatewayStoreRuntime,
) -> io::Result<()> {
    serve_accepted_connection(
        listener.accept_with_envelope_timeout(envelope_timeout)?,
        database_url,
        service_config,
        running_disconnect_policy,
        output_retention,
        maximum_retained_input_bytes,
        transfer_limits,
        object_admission,
        rate_admission,
        disk_reserve,
        disk_probe,
        backends,
        shared_builds,
        shared_build_scheduler,
        gateway_store,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn serve_accepted_connection(
    mut connection: telchar::service::ipc::IpcConnection,
    database_url: &str,
    service_config: &telchar::service::config::ServiceConfig,
    running_disconnect_policy: telchar::service::deployment::RunningDisconnectPolicy,
    output_retention: telchar::service::deployment::OutputRetention,
    maximum_retained_input_bytes: u64,
    transfer_limits: &telchar::service::transfer_limits::TransferLimits,
    object_admission: &telchar::service::transfer_limits::ObjectAdmissionState,
    rate_admission: &telchar::service::transfer_limits::RateAdmissionState,
    disk_reserve: telchar::service::disk_reserve::DiskReserve,
    disk_probe: &dyn telchar::service::disk_reserve::DiskReserveProbe,
    backends: &telchar::backend::routing::ConfiguredBackends,
    shared_builds: &telchar::shared_build::SharedBuildRegistry,
    shared_build_scheduler: &telchar::shared_build::scheduler::SharedBuildScheduler,
    gateway_store: &telchar::store::runtime::GatewayStoreRuntime,
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
    telchar::service::metrics::session_started();
    let result = (|| {
        let input = connection.stream_mut().try_clone()?;
        let mut store_query = gateway_store.query();
        let mut build_executor = backends.executor(database_url)?;
        let mut store_export = gateway_store.export();
        let mut store_import = gateway_store.import()?;
        let mut store_closure = gateway_store.closure();
        let mut store_retention = gateway_store.retention()?;
        let mut store_substitution = gateway_store.substitution();
        let backend_targets = service_config
            .backend_targets()
            .cloned()
            .collect::<Vec<_>>();
        telchar::service::session::SessionBuilder::new(
            input,
            connection.stream_mut().try_clone()?,
            protocol_session_limits(),
        )
        .backend_targets(&backend_targets)
        .disconnect_policy(running_disconnect_policy)
        .retention(output_retention, maximum_retained_input_bytes)
        .stores(
            &mut store_query,
            store_export.as_mut(),
            store_import.as_mut(),
            store_closure.as_mut(),
            store_retention.as_mut(),
            store_substitution.as_mut(),
        )
        .build_executor(&mut build_executor)
        .identity(
            database_url,
            &session_id,
            &connection.envelope().requester.audit_subject,
            &connection.envelope().requester.quota_subject,
        )
        .transfer_admission(transfer_limits, object_admission, rate_admission)
        .disk_admission(disk_reserve, disk_probe)
        .cache_publisher(service_config.cache_publisher())
        .shared_builds(
            shared_builds,
            shared_build_scheduler,
            service_config.scheduling_limits(&connection.envelope().requester.quota_subject),
        )
        .run()
    })();
    telchar::service::metrics::session_finished();
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

pub(super) fn prepare_socket_path(socket: &std::path::Path) -> io::Result<()> {
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

pub(super) struct SessionPermit(Arc<Mutex<usize>>);

impl SessionPermit {
    pub(super) fn acquire(active: Arc<Mutex<usize>>, maximum: usize) -> Option<Self> {
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

pub(super) struct SocketGuard(pub(super) PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
