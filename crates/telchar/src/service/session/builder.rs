use super::*;

pub struct SessionBuilder<'a> {
    input: std::os::unix::net::UnixStream,
    output: std::os::unix::net::UnixStream,
    limits: ProtocolSessionLimits,
    backend_targets: Option<&'a [crate::backend::BackendTarget]>,
    running_disconnect_policy: Option<crate::service::deployment::RunningDisconnectPolicy>,
    output_retention: Option<crate::service::deployment::OutputRetention>,
    maximum_retained_input_bytes: Option<u64>,
    store_query: Option<&'a mut dyn QueryValidPathsStore>,
    build_executor: Option<&'a mut dyn BuildBackend>,
    store_export: Option<&'a mut dyn crate::store::export::StoreExportBackend>,
    store_import: Option<&'a mut dyn crate::store::import::StoreImportBackend>,
    store_closure: Option<&'a mut dyn crate::store::closure::StoreClosureBackend>,
    store_retention: Option<&'a mut dyn crate::store::retention::StoreRetentionBackend>,
    database_url: Option<&'a str>,
    session_id: Option<&'a str>,
    audit_subject: Option<&'a str>,
    quota_subject: Option<&'a str>,
    transfer_limits: Option<&'a crate::service::transfer_limits::TransferLimits>,
    object_admission: Option<&'a crate::service::transfer_limits::ObjectAdmissionState>,
    rate_admission: Option<&'a crate::service::transfer_limits::RateAdmissionState>,
    disk_reserve: Option<crate::service::disk_reserve::DiskReserve>,
    disk_probe: Option<&'a dyn crate::service::disk_reserve::DiskReserveProbe>,
    shared_builds: Option<&'a crate::shared_build::SharedBuildRegistry>,
    shared_build_scheduler: Option<&'a crate::shared_build::scheduler::SharedBuildScheduler>,
    scheduling_limits: Option<crate::service::config::SchedulingLimits>,
}

pub(super) struct SessionContext<'a> {
    pub input: std::os::unix::net::UnixStream,
    pub output: std::os::unix::net::UnixStream,
    pub limits: ProtocolSessionLimits,
    pub backend_targets: &'a [crate::backend::BackendTarget],
    pub running_disconnect_policy: crate::service::deployment::RunningDisconnectPolicy,
    pub output_retention: crate::service::deployment::OutputRetention,
    pub maximum_retained_input_bytes: u64,
    pub store_query: &'a mut dyn QueryValidPathsStore,
    pub build_executor: &'a mut dyn BuildBackend,
    pub store_export: &'a mut dyn crate::store::export::StoreExportBackend,
    pub store_import: &'a mut dyn crate::store::import::StoreImportBackend,
    pub store_closure: &'a mut dyn crate::store::closure::StoreClosureBackend,
    pub store_retention: &'a mut dyn crate::store::retention::StoreRetentionBackend,
    pub database_url: &'a str,
    pub session_id: &'a str,
    pub audit_subject: &'a str,
    pub quota_subject: &'a str,
    pub transfer_limits: &'a crate::service::transfer_limits::TransferLimits,
    pub object_admission: &'a crate::service::transfer_limits::ObjectAdmissionState,
    pub rate_admission: &'a crate::service::transfer_limits::RateAdmissionState,
    pub disk_reserve: crate::service::disk_reserve::DiskReserve,
    pub disk_probe: &'a dyn crate::service::disk_reserve::DiskReserveProbe,
    pub shared_builds: &'a crate::shared_build::SharedBuildRegistry,
    pub shared_build_scheduler: &'a crate::shared_build::scheduler::SharedBuildScheduler,
    pub scheduling_limits: crate::service::config::SchedulingLimits,
}

impl<'a> SessionBuilder<'a> {
    pub fn new(
        input: std::os::unix::net::UnixStream,
        output: std::os::unix::net::UnixStream,
        limits: ProtocolSessionLimits,
    ) -> Self {
        Self {
            input,
            output,
            limits,
            backend_targets: None,
            running_disconnect_policy: None,
            output_retention: None,
            maximum_retained_input_bytes: None,
            store_query: None,
            build_executor: None,
            store_export: None,
            store_import: None,
            store_closure: None,
            store_retention: None,
            database_url: None,
            session_id: None,
            audit_subject: None,
            quota_subject: None,
            transfer_limits: None,
            object_admission: None,
            rate_admission: None,
            disk_reserve: None,
            disk_probe: None,
            shared_builds: None,
            shared_build_scheduler: None,
            scheduling_limits: None,
        }
    }

    pub fn backend_targets(mut self, value: &'a [crate::backend::BackendTarget]) -> Self {
        self.backend_targets = Some(value);
        self
    }
    pub fn disconnect_policy(
        mut self,
        value: crate::service::deployment::RunningDisconnectPolicy,
    ) -> Self {
        self.running_disconnect_policy = Some(value);
        self
    }
    pub fn retention(
        mut self,
        value: crate::service::deployment::OutputRetention,
        maximum_input_bytes: u64,
    ) -> Self {
        self.output_retention = Some(value);
        self.maximum_retained_input_bytes = Some(maximum_input_bytes);
        self
    }
    pub fn stores(
        mut self,
        query: &'a mut dyn QueryValidPathsStore,
        export: &'a mut dyn crate::store::export::StoreExportBackend,
        import: &'a mut dyn crate::store::import::StoreImportBackend,
        closure: &'a mut dyn crate::store::closure::StoreClosureBackend,
        retention: &'a mut dyn crate::store::retention::StoreRetentionBackend,
    ) -> Self {
        self.store_query = Some(query);
        self.store_export = Some(export);
        self.store_import = Some(import);
        self.store_closure = Some(closure);
        self.store_retention = Some(retention);
        self
    }
    pub fn build_executor(mut self, value: &'a mut dyn BuildBackend) -> Self {
        self.build_executor = Some(value);
        self
    }
    pub fn identity(
        mut self,
        database_url: &'a str,
        session_id: &'a str,
        audit_subject: &'a str,
        quota_subject: &'a str,
    ) -> Self {
        self.database_url = Some(database_url);
        self.session_id = Some(session_id);
        self.audit_subject = Some(audit_subject);
        self.quota_subject = Some(quota_subject);
        self
    }
    pub fn transfer_admission(
        mut self,
        limits: &'a crate::service::transfer_limits::TransferLimits,
        objects: &'a crate::service::transfer_limits::ObjectAdmissionState,
        rate: &'a crate::service::transfer_limits::RateAdmissionState,
    ) -> Self {
        self.transfer_limits = Some(limits);
        self.object_admission = Some(objects);
        self.rate_admission = Some(rate);
        self
    }
    pub fn disk_admission(
        mut self,
        reserve: crate::service::disk_reserve::DiskReserve,
        probe: &'a dyn crate::service::disk_reserve::DiskReserveProbe,
    ) -> Self {
        self.disk_reserve = Some(reserve);
        self.disk_probe = Some(probe);
        self
    }
    pub fn shared_builds(
        mut self,
        registry: &'a crate::shared_build::SharedBuildRegistry,
        scheduler: &'a crate::shared_build::scheduler::SharedBuildScheduler,
        limits: crate::service::config::SchedulingLimits,
    ) -> Self {
        self.shared_builds = Some(registry);
        self.shared_build_scheduler = Some(scheduler);
        self.scheduling_limits = Some(limits);
        self
    }

    pub fn run(self) -> io::Result<()> {
        run_worker_session(self.build()?)
    }

    fn build(self) -> io::Result<SessionContext<'a>> {
        let missing = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "worker session is not fully configured",
            )
        };
        Ok(SessionContext {
            input: self.input,
            output: self.output,
            limits: self.limits,
            backend_targets: self.backend_targets.ok_or_else(missing)?,
            running_disconnect_policy: self.running_disconnect_policy.ok_or_else(missing)?,
            output_retention: self.output_retention.ok_or_else(missing)?,
            maximum_retained_input_bytes: self.maximum_retained_input_bytes.ok_or_else(missing)?,
            store_query: self.store_query.ok_or_else(missing)?,
            build_executor: self.build_executor.ok_or_else(missing)?,
            store_export: self.store_export.ok_or_else(missing)?,
            store_import: self.store_import.ok_or_else(missing)?,
            store_closure: self.store_closure.ok_or_else(missing)?,
            store_retention: self.store_retention.ok_or_else(missing)?,
            database_url: self.database_url.ok_or_else(missing)?,
            session_id: self.session_id.ok_or_else(missing)?,
            audit_subject: self.audit_subject.ok_or_else(missing)?,
            quota_subject: self.quota_subject.ok_or_else(missing)?,
            transfer_limits: self.transfer_limits.ok_or_else(missing)?,
            object_admission: self.object_admission.ok_or_else(missing)?,
            rate_admission: self.rate_admission.ok_or_else(missing)?,
            disk_reserve: self.disk_reserve.ok_or_else(missing)?,
            disk_probe: self.disk_probe.ok_or_else(missing)?,
            shared_builds: self.shared_builds.ok_or_else(missing)?,
            shared_build_scheduler: self.shared_build_scheduler.ok_or_else(missing)?,
            scheduling_limits: self.scheduling_limits.ok_or_else(missing)?,
        })
    }
}
