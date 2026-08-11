use std::io;
use std::time::Duration;

use crate::build_request::BuildRequest;

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
        })
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

    fn supports(&self, system: &str, required_features: &[&str]) -> bool {
        self.system == system
            && required_features
                .iter()
                .all(|required| self.features.iter().any(|feature| feature == required))
    }
}

pub fn select_backend<'a>(
    backends: &'a [BackendTarget],
    system: &str,
    required_features: &[&str],
) -> Option<&'a BackendTarget> {
    backends
        .iter()
        .find(|backend| backend.supports(system, required_features))
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildStatus {
    Built,
    AlreadyValid,
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
}

impl BuildResult {
    pub fn new(
        status: BuildStatus,
        outputs: Vec<(Vec<u8>, Vec<u8>)>,
        output_trust: OutputTrust,
    ) -> io::Result<Self> {
        if outputs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "build result has no outputs",
            ));
        }
        Ok(Self {
            status,
            outputs,
            output_trust,
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
    fn execute_with_logs(
        &mut self,
        execution: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult>;

    fn execute(&mut self, execution: &BuildExecution<'_>) -> io::Result<BuildResult> {
        self.execute_with_logs(execution, &mut |_| Ok(()), &mut || Ok(false))
    }
}
