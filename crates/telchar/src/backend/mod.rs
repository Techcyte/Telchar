//! Defines backend capabilities, routing targets, permits, execution requests, logs, and terminal results.

pub mod local;
pub mod routing;
pub mod static_ssh;

use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::build::BuildRequest;

const MAXIMUM_REQUEST_ID_BYTES: usize = 4096;
const MAXIMUM_BACKEND_NAME_BYTES: usize = 256;
const MAXIMUM_SYSTEM_BYTES: usize = 64;
const MAXIMUM_FEATURE_BYTES: usize = 64;
const MAXIMUM_FEATURES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    Local,
    StaticSsh,
    Nomad,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::StaticSsh => "static_ssh",
            Self::Nomad => "nomad",
        }
    }

    pub fn capabilities(self) -> BackendCapabilities {
        match self {
            Self::Local | Self::StaticSsh => BackendCapabilities::new(
                ExecutionRecovery::OutputOnly,
                CancellationCapability::ConnectionBound,
                LogRecovery::LiveOnly,
            ),
            Self::Nomad => BackendCapabilities::new(
                ExecutionRecovery::Adoptable,
                CancellationCapability::Explicit,
                LogRecovery::LiveOnly,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionRecovery {
    OutputOnly,
    Adoptable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationCapability {
    ConnectionBound,
    Explicit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogRecovery {
    LiveOnly,
    Replayable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendCapabilities {
    execution_recovery: ExecutionRecovery,
    cancellation: CancellationCapability,
    log_recovery: LogRecovery,
}

impl BackendCapabilities {
    pub const fn new(
        execution_recovery: ExecutionRecovery,
        cancellation: CancellationCapability,
        log_recovery: LogRecovery,
    ) -> Self {
        Self {
            execution_recovery,
            cancellation,
            log_recovery,
        }
    }

    pub fn execution_recovery(self) -> ExecutionRecovery {
        self.execution_recovery
    }

    pub fn cancellation(self) -> CancellationCapability {
        self.cancellation
    }

    pub fn log_recovery(self) -> LogRecovery {
        self.log_recovery
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendTarget {
    name: String,
    kind: BackendKind,
    system: String,
    features: Vec<String>,
    mandatory_features: Vec<String>,
    selection_priority: u32,
}

impl BackendTarget {
    pub fn new<I, S>(name: &str, kind: BackendKind, system: &str, features: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !valid_component(name, MAXIMUM_BACKEND_NAME_BYTES)
            || !valid_component(system, MAXIMUM_SYSTEM_BYTES)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend target is invalid",
            ));
        }
        let mut normalized = Vec::new();
        for feature in features {
            let feature = feature.as_ref();
            if normalized.len() >= MAXIMUM_FEATURES
                || !valid_component(feature, MAXIMUM_FEATURE_BYTES)
                || normalized.iter().any(|existing| existing == feature)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "backend target is invalid",
                ));
            }
            normalized.push(feature.to_owned());
        }
        Ok(Self {
            name: name.to_owned(),
            kind,
            system: system.to_owned(),
            features: normalized,
            mandatory_features: Vec::new(),
            selection_priority: 1,
        })
    }

    pub fn with_selection_priority(mut self, priority: u32) -> io::Result<Self> {
        if priority == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend target priority is invalid",
            ));
        }
        self.selection_priority = priority;
        Ok(self)
    }

    pub fn with_mandatory_features<I, S>(mut self, features: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for feature in features {
            let feature = feature.as_ref();
            if self.mandatory_features.len() >= MAXIMUM_FEATURES
                || !self.features.iter().any(|supported| supported == feature)
                || self
                    .mandatory_features
                    .iter()
                    .any(|existing| existing == feature)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "backend target is invalid",
                ));
            }
            self.mandatory_features.push(feature.to_owned());
        }
        Ok(self)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> BackendKind {
        self.kind
    }

    pub fn capabilities(&self) -> BackendCapabilities {
        self.kind.capabilities()
    }

    pub fn system(&self) -> &str {
        &self.system
    }

    pub fn features(&self) -> &[String] {
        &self.features
    }

    pub fn mandatory_features(&self) -> &[String] {
        &self.mandatory_features
    }

    pub fn selection_priority(&self) -> u32 {
        self.selection_priority
    }

    pub(crate) fn supports(&self, system: &str, required_features: &[&str]) -> bool {
        (self.system == system || system == "builtin")
            && required_features
                .iter()
                .all(|required| self.features.iter().any(|feature| feature == required))
            && self.mandatory_features.iter().all(|mandatory| {
                required_features
                    .iter()
                    .any(|required| required == mandatory)
            })
    }
}

#[derive(Clone)]
pub struct BackendPool {
    inner: Arc<BackendPoolInner>,
}

#[derive(Debug)]
struct BackendPoolInner {
    targets: Vec<BackendTarget>,
    permits: Arc<Mutex<std::collections::BTreeMap<String, BackendPermits>>>,
    changed: Arc<Condvar>,
}

#[derive(Clone, Copy, Debug)]
struct BackendPermits {
    maximum: usize,
    active: usize,
}

impl BackendPool {
    pub fn new(targets: Vec<BackendTarget>, maximums: Vec<usize>) -> io::Result<Self> {
        if targets.is_empty() || targets.len() != maximums.len() || maximums.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend pool is invalid",
            ));
        }
        let mut names = std::collections::BTreeSet::new();
        if targets.iter().any(|target| !names.insert(target.name())) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "duplicate backend name",
            ));
        }
        for (target, maximum) in targets.iter().zip(&maximums) {
            crate::service::metrics::backend_configured(
                target.name(),
                target.kind().as_str(),
                *maximum as u64,
            );
        }
        Ok(Self {
            inner: Arc::new(BackendPoolInner {
                permits: Arc::new(Mutex::new(
                    targets
                        .iter()
                        .zip(maximums)
                        .map(|(target, maximum)| {
                            (
                                target.name().to_owned(),
                                BackendPermits { maximum, active: 0 },
                            )
                        })
                        .collect(),
                )),
                targets,
                changed: Arc::new(Condvar::new()),
            }),
        })
    }

    pub(crate) fn retain_capacity(self, current: &Self) -> io::Result<Self> {
        let desired = self.inner.permits.lock().unwrap_or_else(|p| p.into_inner());
        let mut retained = current
            .inner
            .permits
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for (name, permits) in desired.iter() {
            if retained
                .get(name)
                .is_some_and(|existing| existing.maximum != permits.maximum)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "backend capacity changed during reload",
                ));
            }
        }
        for (name, permits) in desired.iter() {
            retained.entry(name.clone()).or_insert(*permits);
        }
        Ok(Self {
            inner: Arc::new(BackendPoolInner {
                targets: self.inner.targets.clone(),
                permits: Arc::clone(&current.inner.permits),
                changed: Arc::clone(&current.inner.changed),
            }),
        })
    }

    pub fn targets(&self) -> impl Iterator<Item = &BackendTarget> {
        self.inner.targets.iter()
    }

    pub fn acquire(
        &self,
        system: &str,
        required_features: &[&str],
        timeout: Duration,
    ) -> io::Result<BackendPermit> {
        self.acquire_where(system, required_features, timeout, |_| true)
    }

    pub(crate) fn restore_target(&self, target_name: &str) -> io::Result<BackendPermit> {
        let index = self
            .inner
            .targets
            .iter()
            .position(|target| target.name() == target_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "backend is unavailable"))?;
        let mut permits = self.inner.permits.lock().unwrap_or_else(|p| p.into_inner());
        let selected = permits.get_mut(target_name).expect("configured capacity");
        selected.active = selected
            .active
            .checked_add(1)
            .ok_or_else(|| io::Error::other("backend occupancy overflow"))?;
        let target = &self.inner.targets[index];
        crate::service::metrics::backend_permit_acquired(
            target.name(),
            target.kind().as_str(),
            Duration::ZERO,
        );
        Ok(BackendPermit {
            pool: Arc::clone(&self.inner),
            index,
        })
    }

    pub fn acquire_target(
        &self,
        target_name: &str,
        timeout: Duration,
    ) -> io::Result<BackendPermit> {
        let index = self
            .inner
            .targets
            .iter()
            .position(|target| target.name() == target_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "backend is unavailable"))?;
        self.acquire_index(index, timeout)
    }

    pub fn acquire_where(
        &self,
        system: &str,
        required_features: &[&str],
        timeout: Duration,
        available: impl Fn(&BackendTarget) -> bool,
    ) -> io::Result<BackendPermit> {
        self.acquire_ranked_where(system, required_features, timeout, available, |_| 0)
    }

    pub fn acquire_ranked_where(
        &self,
        system: &str,
        required_features: &[&str],
        timeout: Duration,
        available: impl Fn(&BackendTarget) -> bool,
        locality: impl Fn(&BackendTarget) -> u64,
    ) -> io::Result<BackendPermit> {
        let compatible = self
            .inner
            .targets
            .iter()
            .any(|target| target.supports(system, required_features));
        if !compatible {
            crate::service::metrics::backend_selection(
                None,
                None,
                "failed",
                Some("no_compatible_backend"),
            );
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "compatible backend is unavailable",
            ));
        }
        let ready = self
            .inner
            .targets
            .iter()
            .any(|target| target.supports(system, required_features) && available(target));
        if !ready {
            crate::service::metrics::backend_selection(
                None,
                None,
                "failed",
                Some("backend_unavailable"),
            );
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "compatible backend is not ready",
            ));
        }
        let preferred = self
            .inner
            .targets
            .iter()
            .filter(|target| target.supports(system, required_features) && available(target))
            .fold(None::<&BackendTarget>, |selected, target| match selected {
                Some(current)
                    if (current.selection_priority(), locality(current))
                        >= (target.selection_priority(), locality(target)) =>
                {
                    selected
                }
                _ => Some(target),
            })
            .ok_or_else(|| io::Error::other("compatible backend readiness changed"))?;
        let wait_target_name = preferred.name().to_owned();
        let wait_target_kind = preferred.kind().as_str();
        let started = Instant::now();
        crate::service::metrics::backend_permit_wait_started(&wait_target_name, wait_target_kind);
        let deadline = started.checked_add(timeout).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend permit wait is invalid",
            )
        })?;
        let mut permits = self
            .inner
            .permits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            let index = self
                .inner
                .targets
                .iter()
                .enumerate()
                .filter(|(_, target)| {
                    target.supports(system, required_features)
                        && available(target)
                        && permits[target.name()].active < permits[target.name()].maximum
                })
                .fold(None, |selected, (index, target)| match selected {
                    Some((_, rank)) if rank >= (target.selection_priority(), locality(target)) => {
                        selected
                    }
                    _ => Some((index, (target.selection_priority(), locality(target)))),
                })
                .map(|(index, _)| index);
            if let Some(index) = index {
                permits
                    .get_mut(self.inner.targets[index].name())
                    .expect("configured capacity")
                    .active += 1;
                let target = &self.inner.targets[index];
                crate::service::metrics::backend_selection(
                    Some(target.name()),
                    Some(target.kind().as_str()),
                    "selected",
                    None,
                );
                crate::service::metrics::backend_permit_wait_finished(
                    &wait_target_name,
                    wait_target_kind,
                );
                crate::service::metrics::backend_permit_acquired(
                    target.name(),
                    target.kind().as_str(),
                    started.elapsed(),
                );
                return Ok(BackendPermit {
                    pool: Arc::clone(&self.inner),
                    index,
                });
            }
            let now = Instant::now();
            if now >= deadline {
                crate::service::metrics::backend_selection(
                    None,
                    None,
                    "failed",
                    Some("capacity_timeout"),
                );
                crate::service::metrics::backend_permit_wait_finished(
                    &wait_target_name,
                    wait_target_kind,
                );
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "backend permit wait timed out",
                ));
            }
            let (next, result) = self
                .inner
                .changed
                .wait_timeout(permits, deadline.saturating_duration_since(now))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            permits = next;
            if result.timed_out() {
                crate::service::metrics::backend_selection(
                    None,
                    None,
                    "failed",
                    Some("capacity_timeout"),
                );
                crate::service::metrics::backend_permit_wait_finished(
                    &wait_target_name,
                    wait_target_kind,
                );
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "backend permit wait timed out",
                ));
            }
        }
    }

    fn acquire_index(&self, index: usize, timeout: Duration) -> io::Result<BackendPermit> {
        let started = Instant::now();
        let target_name = self.inner.targets[index].name().to_owned();
        let target_kind = self.inner.targets[index].kind().as_str();
        crate::service::metrics::backend_permit_wait_started(&target_name, target_kind);
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend permit wait is invalid",
            )
        })?;
        let mut permits = self
            .inner
            .permits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            let selected = permits
                .get_mut(self.inner.targets[index].name())
                .expect("configured capacity");
            if selected.active < selected.maximum {
                selected.active += 1;
                let target = &self.inner.targets[index];
                crate::service::metrics::backend_selection(
                    Some(target.name()),
                    Some(target.kind().as_str()),
                    "selected",
                    None,
                );
                crate::service::metrics::backend_permit_wait_finished(&target_name, target_kind);
                crate::service::metrics::backend_permit_acquired(
                    target.name(),
                    target.kind().as_str(),
                    started.elapsed(),
                );
                return Ok(BackendPermit {
                    pool: Arc::clone(&self.inner),
                    index,
                });
            }
            let now = Instant::now();
            if now >= deadline {
                crate::service::metrics::backend_selection(
                    None,
                    None,
                    "failed",
                    Some("capacity_timeout"),
                );
                crate::service::metrics::backend_permit_wait_finished(&target_name, target_kind);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "backend permit wait timed out",
                ));
            }
            let (next, result) = self
                .inner
                .changed
                .wait_timeout(permits, deadline.saturating_duration_since(now))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            permits = next;
            if result.timed_out() {
                crate::service::metrics::backend_selection(
                    None,
                    None,
                    "failed",
                    Some("capacity_timeout"),
                );
                crate::service::metrics::backend_permit_wait_finished(&target_name, target_kind);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "backend permit wait timed out",
                ));
            }
        }
    }
}

#[derive(Debug)]
pub struct BackendPermit {
    pool: Arc<BackendPoolInner>,
    index: usize,
}

impl BackendPermit {
    pub fn target(&self) -> &BackendTarget {
        &self.pool.targets[self.index]
    }
}

impl Drop for BackendPermit {
    fn drop(&mut self) {
        let mut permits = self
            .pool
            .permits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        permits
            .get_mut(self.pool.targets[self.index].name())
            .expect("configured capacity")
            .active -= 1;
        let target = &self.pool.targets[self.index];
        crate::service::metrics::backend_permit_released(target.name(), target.kind().as_str());
        self.pool.changed.notify_all();
    }
}

pub fn select_backend<'a>(
    backends: &'a [BackendTarget],
    system: &str,
    required_features: &[&str],
) -> Option<&'a BackendTarget> {
    backends
        .iter()
        .filter(|backend| backend.supports(system, required_features))
        .fold(None::<&BackendTarget>, |selected, backend| match selected {
            Some(current) if current.selection_priority() >= backend.selection_priority() => {
                selected
            }
            _ => Some(backend),
        })
}

fn valid_component(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+'))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildExecution<'a> {
    request_id: &'a str,
    build: &'a BuildRequest,
    timeout: Duration,
    target_name: Option<String>,
    producers: Vec<crate::persistence::OutputProducer>,
    input_closure: Option<Arc<Vec<crate::store::closure::ClosurePath>>>,
    trace_context: telchar_telemetry::TraceContext,
}

impl<'a> BuildExecution<'a> {
    pub fn new(
        request_id: &'a str,
        build: &'a BuildRequest,
        timeout: Duration,
    ) -> io::Result<Self> {
        if request_id.is_empty()
            || request_id.len() > MAXIMUM_REQUEST_ID_BYTES
            || request_id.contains('\0')
            || timeout.is_zero()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "build execution is invalid",
            ));
        }
        Ok(Self {
            request_id,
            build,
            timeout,
            target_name: None,
            producers: Vec::new(),
            input_closure: None,
            trace_context: telchar_telemetry::TraceContext::default(),
        })
    }

    pub fn request_id(&self) -> &str {
        self.request_id
    }

    pub fn build(&self) -> &BuildRequest {
        self.build
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn set_target_name(&mut self, target_name: &str) -> io::Result<()> {
        if target_name.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend target name is invalid",
            ));
        }
        self.target_name = Some(target_name.to_owned());
        Ok(())
    }

    pub fn target_name(&self) -> Option<&str> {
        self.target_name.as_deref()
    }

    pub fn set_producers(&mut self, producers: Vec<crate::persistence::OutputProducer>) {
        self.producers = producers;
    }

    pub fn producers(&self) -> &[crate::persistence::OutputProducer] {
        &self.producers
    }

    pub fn set_input_closure(
        &mut self,
        closure: Vec<crate::store::closure::ClosurePath>,
    ) -> io::Result<()> {
        crate::store::closure::validate_input_closure(self.build.input_sources(), &closure, true)?;
        self.input_closure = Some(Arc::new(closure));
        Ok(())
    }

    pub fn input_closure(&self) -> Option<&[crate::store::closure::ClosurePath]> {
        self.input_closure.as_deref().map(Vec::as_slice)
    }

    pub(crate) fn retained_input_closure(
        &self,
    ) -> Option<Arc<Vec<crate::store::closure::ClosurePath>>> {
        self.input_closure.clone()
    }

    pub(crate) fn resolve_input_closure(
        &self,
        store: &mut dyn crate::store::closure::StoreClosureBackend,
    ) -> io::Result<std::borrow::Cow<'_, [crate::store::closure::ClosurePath]>> {
        if let Some(closure) = self.input_closure() {
            return Ok(std::borrow::Cow::Borrowed(closure));
        }
        let closure = store.input_closure(self.build.input_sources())?;
        crate::store::closure::validate_input_closure(self.build.input_sources(), &closure, false)?;
        Ok(std::borrow::Cow::Owned(closure))
    }

    pub fn set_trace_context(&mut self, trace_context: telchar_telemetry::TraceContext) {
        self.trace_context = trace_context;
    }

    pub fn trace_context(&self) -> &telchar_telemetry::TraceContext {
        &self.trace_context
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildStatus {
    Built,
    AlreadyValid,
    Failed(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputTrust {
    TrustedExecutor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildResult {
    status: BuildStatus,
    outputs: Vec<(Vec<u8>, Vec<u8>)>,
    output_trust: OutputTrust,
    failure: Option<BuildFailure>,
    gateway_validated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildFailure {
    pub status: u64,
    pub message: String,
}

impl BuildFailure {
    pub fn validate(&self) -> io::Result<()> {
        nix_worker_protocol::validate_build_failure(self.status, &self.message)
    }
}

impl BuildResult {
    pub fn failed(failure: BuildFailure) -> io::Result<Self> {
        failure.validate()?;
        Ok(Self {
            status: BuildStatus::Failed(failure.status),
            outputs: Vec::new(),
            output_trust: OutputTrust::TrustedExecutor,
            failure: Some(failure),
            gateway_validated: false,
        })
    }

    pub(crate) fn mark_gateway_validated(&mut self) {
        self.gateway_validated = true;
    }

    pub(crate) fn gateway_validated(&self) -> bool {
        self.gateway_validated
    }

    pub fn failure(&self) -> Option<&BuildFailure> {
        self.failure.as_ref()
    }

    pub fn new(
        status: BuildStatus,
        outputs: Vec<(Vec<u8>, Vec<u8>)>,
        output_trust: OutputTrust,
    ) -> io::Result<Self> {
        if outputs.is_empty() || matches!(status, BuildStatus::Failed(_)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "build result has no outputs",
            ));
        }
        Ok(Self {
            status,
            outputs,
            output_trust,
            failure: None,
            gateway_validated: false,
        })
    }

    pub fn status(&self) -> BuildStatus {
        self.status
    }

    pub fn outputs(&self) -> &[(Vec<u8>, Vec<u8>)] {
        &self.outputs
    }

    pub fn output_trust(&self) -> OutputTrust {
        self.output_trust
    }
}

pub trait BuildBackend: Send {
    fn execution_id(
        &self,
        _target: &BackendTarget,
        _shared_build_key: &[u8],
    ) -> io::Result<Option<String>> {
        Ok(None)
    }

    fn live_log_queue_bytes(&self, _target: &BackendTarget) -> usize {
        1
    }

    fn compatible_live_log_queue_bytes(&self, _system: &str, _required_features: &[&str]) -> usize {
        1
    }

    fn reserve_target(
        &self,
        _system: &str,
        _required_features: &[&str],
    ) -> io::Result<Option<BackendPermit>> {
        Ok(None)
    }

    fn reserve_build(
        &self,
        execution: &BuildExecution<'_>,
        required_features: &[&str],
    ) -> io::Result<Option<BackendPermit>> {
        self.reserve_target(execution.build().system(), required_features)
    }

    fn selected_target(
        &self,
        _system: &str,
        _required_features: &[&str],
    ) -> io::Result<BackendTarget> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "backend target selection is unavailable",
        ))
    }

    fn execute_with_logs(
        &mut self,
        execution: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult>;

    fn execute_reserved(
        &mut self,
        execution: &BuildExecution<'_>,
        permit: BackendPermit,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        drop(permit);
        self.execute_with_logs(execution, logs, cancelled)
    }

    fn execute(&mut self, execution: &BuildExecution<'_>) -> io::Result<BuildResult> {
        self.execute_with_logs(execution, &mut |_| Ok(()), &mut || Ok(false))
    }
}

#[cfg(test)]
pub(crate) mod retained_input_tests {
    use super::*;
    use crate::store::closure::{ClosurePath, StoreClosureBackend};

    pub(crate) fn request() -> BuildRequest {
        let target =
            BackendTarget::new("nomad", BackendKind::Nomad, "x86_64-linux", [] as [&str; 0])
                .unwrap();
        BuildRequest::from_stored_derivation(
            b"/nix/store/22222222222222222222222222222222-test.drv",
            br#"Derive([("out","/nix/store/11111111111111111111111111111111-test","","")],[],["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root"],"x86_64-linux","/bin/sh",[],[("out","/nix/store/11111111111111111111111111111111-test")])"#,
            &[target],
        ).unwrap()
    }

    pub(crate) fn metadata() -> Vec<ClosurePath> {
        vec![ClosurePath {
            store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root".into(),
            nar_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            nar_size: 42,
            references: vec![],
            deriver: None,
            content_address: None,
        }]
    }

    struct CountingClosure {
        queries: usize,
        paths: Vec<ClosurePath>,
    }
    impl StoreClosureBackend for CountingClosure {
        fn input_closure(&mut self, _: &[Vec<u8>]) -> io::Result<Vec<ClosurePath>> {
            self.queries += 1;
            Ok(self.paths.clone())
        }
    }

    #[test]
    fn retained_input_closure_skips_queries_and_clones_share_metadata() {
        let request = request();
        let mut execution =
            BuildExecution::new("request", &request, Duration::from_secs(1)).unwrap();
        execution.set_input_closure(metadata()).unwrap();
        let cloned = execution.clone();
        assert!(std::ptr::eq(
            execution.input_closure().unwrap(),
            cloned.input_closure().unwrap()
        ));
        let mut store = CountingClosure {
            queries: 0,
            paths: metadata(),
        };
        for _ in 0..2 {
            assert_eq!(
                execution
                    .resolve_input_closure(&mut store)
                    .unwrap()
                    .as_ref(),
                metadata()
            );
        }
        assert_eq!(
            store.queries, 0,
            "active execution must not traverse retained inputs again"
        );
    }

    #[test]
    fn reconstruction_expands_manual_roots_but_retained_metadata_requires_normalized_inputs() {
        let request = request();
        let mut paths = metadata();
        let mut reference = paths[0].clone();
        reference.store_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-reference".into();
        paths[0].references.push(reference.store_path.clone());
        paths.push(reference);
        let mut execution =
            BuildExecution::new("request", &request, Duration::from_secs(1)).unwrap();
        assert!(
            execution.set_input_closure(paths.clone()).is_err(),
            "extra paths require admission normalization"
        );
        assert!(execution.input_closure().is_none());
        let mut store = CountingClosure {
            queries: 0,
            paths: paths.clone(),
        };
        assert_eq!(
            execution
                .resolve_input_closure(&mut store)
                .unwrap()
                .as_ref(),
            paths
        );
        assert_eq!(store.queries, 1);
        let mut normalized = request.clone();
        normalized.set_input_closure(
            paths
                .iter()
                .map(|path| path.store_path.as_bytes().to_vec())
                .collect(),
        );
        let mut execution =
            BuildExecution::new("request", &normalized, Duration::from_secs(1)).unwrap();
        let mut missing_reference = paths.clone();
        missing_reference.pop();
        assert!(execution.set_input_closure(missing_reference).is_err());
        execution.set_input_closure(paths.clone()).unwrap();
        assert_eq!(
            execution
                .resolve_input_closure(&mut store)
                .unwrap()
                .as_ref(),
            paths
        );
        assert_eq!(store.queries, 1);
    }

    #[test]
    fn duplicate_admitted_roots_cannot_authorize_foreign_retained_metadata() {
        let request = request();
        let mut serialized = serde_json::to_value(&request).unwrap();
        serialized["input_sources"] =
            serde_json::json!([request.input_sources()[0], request.input_sources()[0]]);
        let request = serde_json::from_value(serialized).unwrap();
        let mut paths = metadata();
        let mut foreign = paths[0].clone();
        foreign.store_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-foreign".into();
        paths.push(foreign);
        let mut execution =
            BuildExecution::new("request", &request, Duration::from_secs(1)).unwrap();
        assert!(execution.set_input_closure(paths).is_err());
        assert!(execution.input_closure().is_none());
    }

    #[test]
    fn absent_input_closure_reconstructs_and_incomplete_metadata_fails_closed() {
        let request = request();
        let mut execution =
            BuildExecution::new("request", &request, Duration::from_secs(1)).unwrap();
        let mut store = CountingClosure {
            queries: 0,
            paths: metadata(),
        };
        assert_eq!(
            execution
                .resolve_input_closure(&mut store)
                .unwrap()
                .as_ref(),
            metadata()
        );
        assert_eq!(store.queries, 1);
        store.paths.clear();
        assert!(execution.resolve_input_closure(&mut store).is_err());
        assert!(execution.set_input_closure(vec![]).is_err());
        let mut invalid = metadata();
        invalid[0].nar_size = 0;
        assert!(execution.set_input_closure(invalid).is_err());
        let mut invalid = metadata();
        invalid[0]
            .references
            .push("/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-absent".into());
        assert!(execution.set_input_closure(invalid).is_err());
        let mut invalid = metadata();
        invalid.push(invalid[0].clone());
        assert!(execution.set_input_closure(invalid).is_err());
    }
}
