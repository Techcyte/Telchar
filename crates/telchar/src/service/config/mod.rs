//! Parses and validates strict service configuration for identity, scheduling, stores, and every backend kind.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::backend::{BackendKind, BackendTarget};
use crate::service::deployment::{OutputRetention, RunningDisconnectPolicy};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/telchar/telchar.toml";
const DEFAULT_MAXIMUM_IPC_SESSIONS: usize = 256;
const MAXIMUM_IPC_SESSIONS: usize = 65_536;
const DEFAULT_OWNERSHIP_RENEWAL_SECONDS: u64 = 5;
const DEFAULT_OWNERSHIP_LEASE_SECONDS: u64 = 20;
const MAXIMUM_OWNERSHIP_LEASE_SECONDS: u64 = 24 * 60 * 60;
const DEFAULT_NOMAD_CALLBACK_BIND: &str = "0.0.0.0:7443";
const DEFAULT_NOMAD_CALLBACK_PUBLIC_URL: &str = "ws://127.0.0.1:7443/callback";
const DEFAULT_NOMAD_CALLBACK_MAXIMUM_CONNECTIONS: usize = 64;
const DEFAULT_NOMAD_CALLBACK_MAXIMUM_HEADER_BYTES: usize = 16 * 1024;
const DEFAULT_NOMAD_CALLBACK_MAXIMUM_BODY_BYTES: usize = 64 * 1024;
const DEFAULT_NOMAD_CALLBACK_AUTHENTICATION_REQUEST_TIMEOUT_SECONDS: u64 = 10;
const DEFAULT_NOMAD_CALLBACK_SHUTDOWN_DRAIN_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_NOMAD_CALLBACK_MAXIMUM_JWKS_BYTES: usize = 1024 * 1024;
const DEFAULT_NOMAD_CALLBACK_MAXIMUM_RETAINED_NONCES: usize = 65_536;
const MAXIMUM_NOMAD_CALLBACK_CONNECTIONS: usize = 65_536;
const MAXIMUM_NOMAD_CALLBACK_HEADER_BYTES: usize = 1024 * 1024;
const MAXIMUM_NOMAD_CALLBACK_BODY_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_CREDENTIAL_MAPPINGS: usize = 4_096;
const MAXIMUM_CREDENTIAL_ID_BYTES: usize = 1_024;
const MAXIMUM_SUBJECT_BYTES: usize = 256;
const MAXIMUM_STATIC_SSH_BACKENDS: usize = 256;
const MAXIMUM_NOMAD_BACKENDS: usize = 256;
const MAXIMUM_NOMAD_CONSTRAINTS: usize = 64;
const MAXIMUM_NOMAD_CONSTRAINT_FIELD_BYTES: usize = 1_024;
const MAXIMUM_BACKEND_CONCURRENT_BUILDS: usize = 65_536;
const MAXIMUM_NOMAD_ENDPOINT_BYTES: usize = 2_048;
const MAXIMUM_NOMAD_DRIVER_CONFIG_BYTES: usize = 16 * 1024;
const MAXIMUM_NOMAD_DRIVER_CONFIG_ENTRIES: usize = 256;
const MAXIMUM_NOMAD_DRIVER_CONFIG_DEPTH: usize = 4;
const MAXIMUM_NOMAD_RESOURCE: u64 = 16 * 1024 * 1024;
const MAXIMUM_NOMAD_POLL_INTERVAL_SECONDS: u64 = 300;
const MAXIMUM_NOMAD_RUNTIME_LIMIT_SECONDS: u64 = 7 * 24 * 60 * 60;
const MAXIMUM_NOMAD_TRANSFER_PATHS: usize = 1_000_000;
const MAXIMUM_NOMAD_TRANSFER_BYTES: u64 = i64::MAX as u64;
const MAXIMUM_NOMAD_TRANSFER_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_NOMAD_TRANSFER_TIMEOUT_SECONDS: u64 = 24 * 60 * 60;
const MAXIMUM_NOMAD_AUTHENTICATION_SECONDS: u64 = 60 * 60;
const MAXIMUM_NOMAD_NONCE_RETENTION_SECONDS: u64 = 24 * 60 * 60;
const MAXIMUM_NOMAD_STORE_URI_BYTES: usize = 2_048;
const DEFAULT_MAXIMUM_QUEUED_BUILDS: usize = 65_536;
const DEFAULT_MAXIMUM_ACTIVE_BUILDS: usize = 65_536;
const MAXIMUM_SCHEDULING_BUILDS: usize = 65_536;
const MAXIMUM_QUOTA_SUBJECTS: usize = 4_096;
const DEFAULT_BACKEND_PERMIT_WAIT_SECONDS: u64 = 30;
const MAXIMUM_BACKEND_PERMIT_WAIT_SECONDS: u64 = 3_600;
const MAXIMUM_SSH_DESTINATION_BYTES: usize = 512;
const DEFAULT_STATIC_SSH_READY_CHECK_INTERVAL_SECONDS: u64 = 5 * 60;
const DEFAULT_STATIC_SSH_UNAVAILABLE_CHECK_INTERVAL_SECONDS: u64 = 60;
const DEFAULT_STATIC_SSH_CHECK_TIMEOUT_SECONDS: u64 = 10;
const MAXIMUM_STATIC_SSH_CHECK_INTERVAL_SECONDS: u64 = 24 * 60 * 60;
const MAXIMUM_STATIC_SSH_CHECK_TIMEOUT_SECONDS: u64 = 5 * 60;
const SYSTEM_SSH_PROGRAM: &str = "/usr/bin/ssh";
const PACKAGED_SSH_PROGRAM: Option<&str> = option_env!("TELCHAR_DEFAULT_SSH_PROGRAM");

#[derive(Clone, Debug, PartialEq)]
pub struct ServiceConfig {
    running_disconnect_policy: RunningDisconnectPolicy,
    output_retention: OutputRetention,
    maximum_retained_input_bytes: u64,
    cache_publisher: Option<crate::service::cache_publication::CachePublisher>,
    database_url: Option<String>,
    ownership_renewal_interval: Duration,
    ownership_lease_duration: Duration,
    ipc_socket: Option<PathBuf>,
    maximum_ipc_sessions: usize,
    nomad_callback: NomadCallbackConfig,
    credential_mappings: BTreeMap<String, CredentialMapping>,
    default_scheduling_limits: SchedulingLimits,
    subject_scheduling_limits: BTreeMap<String, SchedulingLimits>,
    backend_permit_wait: Duration,
    local_backend: Option<LocalBackendConfig>,
    static_ssh_backends: Vec<StaticSshBackendConfig>,
    nomad_backends: Vec<NomadBackendConfig>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticSshReloadChanges {
    pub added: usize,
    pub removed: usize,
}

impl ServiceConfig {
    pub fn load() -> io::Result<Self> {
        match std::env::var_os("TELCHAR_CONFIG") {
            Some(path) => Self::load_path(Path::new(&path), true),
            None => Self::load_from_default(Path::new(DEFAULT_CONFIG_PATH)),
        }
    }

    pub fn load_from_default(path: &Path) -> io::Result<Self> {
        Self::load_path(path, false)
    }

    pub fn running_disconnect_policy(&self) -> RunningDisconnectPolicy {
        self.running_disconnect_policy
    }

    pub fn output_retention(&self) -> OutputRetention {
        self.output_retention
    }

    pub fn maximum_retained_input_bytes(&self) -> u64 {
        self.maximum_retained_input_bytes
    }

    pub fn cache_publisher(&self) -> Option<&crate::service::cache_publication::CachePublisher> {
        self.cache_publisher.as_ref()
    }

    pub fn database_url(&self) -> Option<&str> {
        self.database_url.as_deref()
    }

    pub fn require_database_url(&self) -> io::Result<&str> {
        self.database_url
            .as_deref()
            .ok_or_else(|| invalid("database URL is not configured"))
    }

    pub fn ownership_renewal_interval(&self) -> Duration {
        self.ownership_renewal_interval
    }

    pub fn ownership_lease_duration(&self) -> Duration {
        self.ownership_lease_duration
    }

    pub fn ipc_socket(&self) -> Option<&Path> {
        self.ipc_socket.as_deref()
    }

    pub fn require_ipc_socket(&self) -> io::Result<&Path> {
        self.ipc_socket
            .as_deref()
            .ok_or_else(|| invalid("IPC socket is not configured"))
    }

    pub fn maximum_ipc_sessions(&self) -> usize {
        self.maximum_ipc_sessions
    }

    pub fn nomad_callback(&self) -> &NomadCallbackConfig {
        &self.nomad_callback
    }

    pub fn credential_mapping(&self, credential_id: &str) -> Option<&CredentialMapping> {
        self.credential_mappings.get(credential_id)
    }

    pub fn scheduling_limits(&self, quota_subject: &str) -> SchedulingLimits {
        self.subject_scheduling_limits
            .get(quota_subject)
            .copied()
            .unwrap_or(self.default_scheduling_limits)
    }

    pub fn backend_permit_wait(&self) -> Duration {
        self.backend_permit_wait
    }

    pub fn local_backend(&self) -> Option<&LocalBackendConfig> {
        self.local_backend.as_ref()
    }

    pub fn static_ssh_backends(&self) -> &[StaticSshBackendConfig] {
        &self.static_ssh_backends
    }

    pub fn nomad_backends(&self) -> &[NomadBackendConfig] {
        &self.nomad_backends
    }

    pub fn validate_static_ssh_reload(
        &self,
        replacement: &Self,
    ) -> io::Result<StaticSshReloadChanges> {
        if self.running_disconnect_policy != replacement.running_disconnect_policy
            || self.output_retention != replacement.output_retention
            || self.maximum_retained_input_bytes != replacement.maximum_retained_input_bytes
            || self.cache_publisher != replacement.cache_publisher
            || self.database_url != replacement.database_url
            || self.ownership_renewal_interval != replacement.ownership_renewal_interval
            || self.ownership_lease_duration != replacement.ownership_lease_duration
            || self.ipc_socket != replacement.ipc_socket
            || self.maximum_ipc_sessions != replacement.maximum_ipc_sessions
            || self.nomad_callback != replacement.nomad_callback
            || self.credential_mappings != replacement.credential_mappings
            || self.default_scheduling_limits != replacement.default_scheduling_limits
            || self.subject_scheduling_limits != replacement.subject_scheduling_limits
            || self.backend_permit_wait != replacement.backend_permit_wait
            || self.local_backend != replacement.local_backend
            || self.nomad_backends != replacement.nomad_backends
        {
            return Err(invalid("configuration reload changes immutable settings"));
        }
        for backend in &self.static_ssh_backends {
            if let Some(candidate) = replacement
                .static_ssh_backends
                .iter()
                .find(|candidate| candidate.target().name() == backend.target().name())
                && candidate != backend
            {
                return Err(invalid(
                    "configuration reload changes an existing static SSH backend",
                ));
            }
        }
        let added = replacement
            .static_ssh_backends
            .iter()
            .filter(|backend| {
                !self
                    .static_ssh_backends
                    .iter()
                    .any(|current| current.target().name() == backend.target().name())
            })
            .count();
        let removed = self
            .static_ssh_backends
            .iter()
            .filter(|backend| {
                !replacement
                    .static_ssh_backends
                    .iter()
                    .any(|candidate| candidate.target().name() == backend.target().name())
            })
            .count();
        Ok(StaticSshReloadChanges { added, removed })
    }

    pub fn backend_targets(&self) -> impl Iterator<Item = &BackendTarget> {
        self.local_backend
            .iter()
            .map(LocalBackendConfig::target)
            .chain(
                self.static_ssh_backends
                    .iter()
                    .map(StaticSshBackendConfig::target),
            )
            .chain(self.nomad_backends.iter().map(NomadBackendConfig::target))
    }

    pub fn system_features(&self) -> BTreeMap<&str, std::collections::BTreeSet<&str>> {
        let mut systems: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
        for target in self.backend_targets() {
            let features = systems.entry(target.system()).or_default();
            features.extend(target.features().iter().map(String::as_str));
        }
        systems
    }

    fn load_path(path: &Path, required: bool) -> io::Result<Self> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => RawServiceConfig::parse(&raw)?,
            Err(error) if !required && error.kind() == io::ErrorKind::NotFound => {
                RawServiceConfig::default()
            }
            Err(_) => return Err(invalid("service configuration could not be read")),
        };
        Self::build(raw)
    }

    fn build(mut raw: RawServiceConfig) -> io::Result<Self> {
        let replacement_mapping_file = environment_path("TELCHAR_IDENTITY_MAPPINGS_FILE")?;
        if let Some(path) = replacement_mapping_file {
            let contents = fs::read_to_string(path)
                .map_err(|_| invalid("identity mapping file could not be read"))?;
            raw.identity = Some(IdentityConfig {
                credentials: RawIdentityConfig::parse(&contents)?.credentials,
            });
        }

        let output_retention = match environment_string("TELCHAR_OUTPUT_RETENTION_SECONDS")? {
            Some(value) => parse_retention(&value)?,
            None => raw
                .output_retention_seconds
                .map(|seconds| parse_retention(&seconds.to_string()))
                .transpose()?
                .unwrap_or_default(),
        };
        let maximum_retained_input_bytes =
            match environment_string("TELCHAR_MAX_RETAINED_INPUT_BYTES")? {
                Some(value) => parse_positive_u64(&value, "retained input byte limit is invalid")?,
                None => raw
                    .maximum_retained_input_bytes
                    .unwrap_or(crate::service::deployment::DEFAULT_MAXIMUM_RETAINED_INPUT_BYTES),
            };
        if maximum_retained_input_bytes > i64::MAX as u64 {
            return Err(invalid("retained input byte limit is invalid"));
        }

        let cache_publisher = raw
            .cache_publication
            .map(|config| {
                crate::service::cache_publication::CachePublisher::new(
                    config.executable,
                    config.arguments,
                    Duration::from_secs(config.timeout_seconds.unwrap_or(300)),
                    config.maximum_input_bytes.unwrap_or(64 * 1024),
                )
            })
            .transpose()?;

        let running_disconnect_policy = environment_string("TELCHAR_RUNNING_DISCONNECT_POLICY")?
            .or(raw.running_disconnect_policy)
            .map(|value| RunningDisconnectPolicy::parse(&value))
            .transpose()?
            .unwrap_or_default();

        let database = raw.database;
        let database_url = match environment_string("TELCHAR_DATABASE_URL")? {
            Some(value) => Some(nonempty(value, "database URL is invalid")?),
            None => database
                .as_ref()
                .and_then(|database| database.url_file.clone())
                .map(read_secret)
                .transpose()?,
        };
        let ownership_renewal_seconds = database
            .as_ref()
            .and_then(|database| database.ownership_renewal_seconds)
            .unwrap_or(DEFAULT_OWNERSHIP_RENEWAL_SECONDS);
        let ownership_lease_seconds = database
            .as_ref()
            .and_then(|database| database.ownership_lease_seconds)
            .unwrap_or(DEFAULT_OWNERSHIP_LEASE_SECONDS);
        if ownership_renewal_seconds == 0
            || ownership_lease_seconds > MAXIMUM_OWNERSHIP_LEASE_SECONDS
            || ownership_lease_seconds < ownership_renewal_seconds.saturating_mul(3)
        {
            return Err(invalid("database ownership durations are invalid"));
        }
        let ipc_socket = environment_path("TELCHAR_IPC_SOCKET")?
            .or_else(|| raw.ipc.as_ref().and_then(|ipc| ipc.socket.clone()));
        if ipc_socket.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(invalid("IPC socket path is invalid"));
        }
        let maximum_ipc_sessions = match environment_string("TELCHAR_IPC_MAX_SESSIONS")? {
            Some(value) => parse_positive_usize(&value, "IPC session limit is invalid")?,
            None => raw
                .ipc
                .and_then(|ipc| ipc.maximum_sessions)
                .unwrap_or(DEFAULT_MAXIMUM_IPC_SESSIONS),
        };
        if maximum_ipc_sessions > MAXIMUM_IPC_SESSIONS {
            return Err(invalid("IPC session limit is invalid"));
        }

        let nomad_callback = validate_nomad_callback(raw.nomad_callback.unwrap_or_default())?;
        let credential_mappings = validate_mappings(
            raw.identity
                .map(|identity| identity.credentials)
                .unwrap_or_default(),
        )?;
        let scheduling = raw.scheduling.unwrap_or_default();
        if scheduling.subjects.len() > MAXIMUM_QUOTA_SUBJECTS {
            return Err(invalid("scheduling subject count exceeds limit"));
        }
        let default_scheduling_limits =
            validate_scheduling_limits(scheduling.default.unwrap_or(RawSchedulingLimits {
                maximum_queued_builds: DEFAULT_MAXIMUM_QUEUED_BUILDS,
                maximum_active_builds: DEFAULT_MAXIMUM_ACTIVE_BUILDS,
            }))?;
        let subject_scheduling_limits = scheduling
            .subjects
            .into_iter()
            .map(|(subject, limits)| {
                let subject = validate_subject(subject, "quota subject is invalid")?;
                Ok((subject, validate_scheduling_limits(limits)?))
            })
            .collect::<io::Result<BTreeMap<_, _>>>()?;
        let backends = raw.backends.unwrap_or_default();
        let backend_permit_wait_seconds = backends
            .permit_wait_seconds
            .unwrap_or(DEFAULT_BACKEND_PERMIT_WAIT_SECONDS);
        if backend_permit_wait_seconds == 0
            || backend_permit_wait_seconds > MAXIMUM_BACKEND_PERMIT_WAIT_SECONDS
        {
            return Err(invalid("backend permit wait is invalid"));
        }
        let local_backend = backends.local.map(validate_local_backend).transpose()?;
        let static_ssh_backends = validate_static_ssh_backends(backends.static_ssh)?;
        let nomad_backends = validate_nomad_backends(backends.nomad, nomad_callback.public_url())?;
        validate_unique_backend_names(
            local_backend.as_ref(),
            &static_ssh_backends,
            &nomad_backends,
        )?;
        Ok(Self {
            running_disconnect_policy,
            output_retention,
            maximum_retained_input_bytes,
            cache_publisher,
            database_url,
            ownership_renewal_interval: Duration::from_secs(ownership_renewal_seconds),
            ownership_lease_duration: Duration::from_secs(ownership_lease_seconds),
            ipc_socket,
            maximum_ipc_sessions,
            nomad_callback,
            credential_mappings,
            default_scheduling_limits,
            subject_scheduling_limits,
            backend_permit_wait: Duration::from_secs(backend_permit_wait_seconds),
            local_backend,
            static_ssh_backends,
            nomad_backends,
        })
    }
}

mod model;
mod raw;
mod validation;

pub use model::*;
use raw::*;
use validation::*;

mod helpers;

use helpers::*;
