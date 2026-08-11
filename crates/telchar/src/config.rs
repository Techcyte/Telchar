use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::backend::{BackendKind, BackendTarget};
use crate::deployment::{DeploymentConfig, OutputRetention, RunningDisconnectPolicy};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/telchar/telchar.toml";
const DEFAULT_MAXIMUM_IPC_SESSIONS: usize = 64;
const MAXIMUM_IPC_SESSIONS: usize = 65_536;
const MAXIMUM_CREDENTIAL_MAPPINGS: usize = 4_096;
const MAXIMUM_CREDENTIAL_ID_BYTES: usize = 1_024;
const MAXIMUM_SUBJECT_BYTES: usize = 256;
const MAXIMUM_STATIC_SSH_BACKENDS: usize = 256;
const MAXIMUM_BACKEND_CONCURRENT_BUILDS: usize = 65_536;
const DEFAULT_BACKEND_PERMIT_WAIT_SECONDS: u64 = 30;
const MAXIMUM_BACKEND_PERMIT_WAIT_SECONDS: u64 = 3_600;
const MAXIMUM_SSH_DESTINATION_BYTES: usize = 512;
const SYSTEM_SSH_PROGRAM: &str = "/usr/bin/ssh";
const PACKAGED_SSH_PROGRAM: Option<&str> = option_env!("TELCHAR_DEFAULT_SSH_PROGRAM");

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialMapping {
    pub audit_subject: Option<String>,
    pub quota_subject: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalBackendConfig {
    target: BackendTarget,
    maximum_concurrent_builds: usize,
}

impl LocalBackendConfig {
    pub fn target(&self) -> &BackendTarget {
        &self.target
    }

    pub fn maximum_concurrent_builds(&self) -> usize {
        self.maximum_concurrent_builds
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaticSshBackendConfig {
    target: BackendTarget,
    maximum_concurrent_builds: usize,
    destination: String,
    identity_file: PathBuf,
    known_hosts_file: PathBuf,
    ssh_program: PathBuf,
}

impl StaticSshBackendConfig {
    pub fn target(&self) -> &BackendTarget {
        &self.target
    }

    pub fn maximum_concurrent_builds(&self) -> usize {
        self.maximum_concurrent_builds
    }

    pub fn destination(&self) -> &str {
        &self.destination
    }

    pub fn identity_file(&self) -> &Path {
        &self.identity_file
    }

    pub fn known_hosts_file(&self) -> &Path {
        &self.known_hosts_file
    }

    pub fn ssh_program(&self) -> &Path {
        &self.ssh_program
    }
}

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    deployment: Option<DeploymentConfig>,
    running_disconnect_policy: RunningDisconnectPolicy,
    database_url: Option<String>,
    ipc_socket: Option<PathBuf>,
    maximum_ipc_sessions: usize,
    credential_mappings: BTreeMap<String, CredentialMapping>,
    backend_permit_wait: Duration,
    local_backend: Option<LocalBackendConfig>,
    static_ssh_backends: Vec<StaticSshBackendConfig>,
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

    pub fn deployment(&self) -> &DeploymentConfig {
        self.deployment
            .as_ref()
            .expect("deployment configuration is required")
    }

    pub fn require_deployment(&self) -> io::Result<&DeploymentConfig> {
        self.deployment
            .as_ref()
            .ok_or_else(|| invalid("deployment system is not configured"))
    }

    pub fn deployment_option(&self) -> Option<&DeploymentConfig> {
        self.deployment.as_ref()
    }

    pub fn running_disconnect_policy(&self) -> RunningDisconnectPolicy {
        self.running_disconnect_policy
    }

    pub fn database_url(&self) -> Option<&str> {
        self.database_url.as_deref()
    }

    pub fn require_database_url(&self) -> io::Result<&str> {
        self.database_url
            .as_deref()
            .ok_or_else(|| invalid("database URL is not configured"))
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

    pub fn credential_mapping(&self, credential_id: &str) -> Option<&CredentialMapping> {
        self.credential_mappings.get(credential_id)
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

    pub fn backend_targets(&self) -> impl Iterator<Item = &BackendTarget> {
        self.local_backend
            .iter()
            .map(LocalBackendConfig::target)
            .chain(
                self.static_ssh_backends
                    .iter()
                    .map(StaticSshBackendConfig::target),
            )
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

        let system = environment_string("TELCHAR_SYSTEM")?.or_else(|| {
            raw.deployment
                .as_ref()
                .and_then(|value| value.system.clone())
        });
        let features_override = environment_string("TELCHAR_SUPPORTED_FEATURES")?;
        let features = match features_override {
            Some(value) => value,
            None => raw
                .deployment
                .as_ref()
                .and_then(|value| value.supported_features.as_ref())
                .map(|values| values.join(","))
                .unwrap_or_default(),
        };
        let retention = match environment_string("TELCHAR_OUTPUT_RETENTION_SECONDS")? {
            Some(value) => Some(parse_retention(&value)?),
            None => raw
                .deployment
                .as_ref()
                .and_then(|value| value.output_retention_seconds)
                .map(|seconds| parse_retention(&seconds.to_string()))
                .transpose()?,
        };
        let maximum_retained_input_bytes =
            match environment_string("TELCHAR_MAX_RETAINED_INPUT_BYTES")? {
                Some(value) => parse_positive_u64(&value, "retained input byte limit is invalid")?,
                None => raw
                    .deployment
                    .as_ref()
                    .and_then(|value| value.maximum_retained_input_bytes)
                    .unwrap_or(crate::deployment::DEFAULT_MAXIMUM_RETAINED_INPUT_BYTES),
            };
        if maximum_retained_input_bytes > i64::MAX as u64 {
            return Err(invalid("retained input byte limit is invalid"));
        }
        let deployment: Option<DeploymentConfig> = system
            .map(|system| {
                let mut deployment = DeploymentConfig::parse(&system, &features)?;
                if let Some(retention) = retention {
                    deployment.set_output_retention(retention);
                }
                deployment.set_maximum_retained_input_bytes(maximum_retained_input_bytes);
                Ok::<DeploymentConfig, io::Error>(deployment)
            })
            .transpose()?;

        let running_disconnect_policy = environment_string("TELCHAR_RUNNING_DISCONNECT_POLICY")?
            .or_else(|| {
                raw.deployment
                    .as_ref()
                    .and_then(|value| value.running_disconnect_policy.clone())
            })
            .map(|value| RunningDisconnectPolicy::parse(&value))
            .transpose()?
            .unwrap_or_default();

        let database_url = match environment_string("TELCHAR_DATABASE_URL")? {
            Some(value) => Some(nonempty(value, "database URL is invalid")?),
            None => raw
                .database
                .and_then(|database| database.url_file)
                .map(read_secret)
                .transpose()?,
        };
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

        let credential_mappings = validate_mappings(
            raw.identity
                .map(|identity| identity.credentials)
                .unwrap_or_default(),
        )?;
        let mut backends = raw.backends.unwrap_or_default();
        if backends.local.is_none()
            && backends.static_ssh.is_empty()
            && let Some(deployment) = &deployment
        {
            backends.local = Some(RawLocalBackendConfig {
                name: "local".to_owned(),
                system: deployment.system().to_owned(),
                supported_features: deployment.supported_features().to_vec(),
                maximum_concurrent_builds: 1,
            });
        }
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
        Ok(Self {
            deployment,
            running_disconnect_policy,
            database_url,
            ipc_socket,
            maximum_ipc_sessions,
            credential_mappings,
            backend_permit_wait: Duration::from_secs(backend_permit_wait_seconds),
            local_backend,
            static_ssh_backends,
        })
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServiceConfig {
    deployment: Option<DeploymentSection>,
    database: Option<DatabaseSection>,
    ipc: Option<IpcSection>,
    identity: Option<IdentityConfig>,
    backends: Option<BackendConfig>,
}

impl RawServiceConfig {
    fn parse(raw: &str) -> io::Result<Self> {
        toml::from_str(raw).map_err(|_| invalid("service configuration is invalid"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentSection {
    system: Option<String>,
    supported_features: Option<Vec<String>>,
    running_disconnect_policy: Option<String>,
    output_retention_seconds: Option<u64>,
    maximum_retained_input_bytes: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DatabaseSection {
    url_file: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IpcSection {
    socket: Option<PathBuf>,
    maximum_sessions: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityConfig {
    #[serde(default)]
    credentials: BTreeMap<String, RawCredentialMapping>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIdentityConfig {
    #[serde(default)]
    credentials: BTreeMap<String, RawCredentialMapping>,
}

impl RawIdentityConfig {
    fn parse(raw: &str) -> io::Result<Self> {
        toml::from_str(raw).map_err(|_| invalid("identity mapping file is invalid"))
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendConfig {
    permit_wait_seconds: Option<u64>,
    local: Option<RawLocalBackendConfig>,
    #[serde(default)]
    static_ssh: Vec<RawStaticSshBackendConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocalBackendConfig {
    name: String,
    system: String,
    #[serde(default)]
    supported_features: Vec<String>,
    maximum_concurrent_builds: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStaticSshBackendConfig {
    name: String,
    system: String,
    #[serde(default)]
    supported_features: Vec<String>,
    maximum_concurrent_builds: usize,
    destination: String,
    identity_file: PathBuf,
    known_hosts_file: PathBuf,
    ssh_program: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCredentialMapping {
    audit_subject: Option<String>,
    quota_subject: Option<String>,
}

fn validate_mappings(
    raw: BTreeMap<String, RawCredentialMapping>,
) -> io::Result<BTreeMap<String, CredentialMapping>> {
    if raw.len() > MAXIMUM_CREDENTIAL_MAPPINGS {
        return Err(invalid("credential mapping count exceeds limit"));
    }
    raw.into_iter()
        .map(|(credential_id, mapping)| {
            if credential_id.is_empty()
                || credential_id.len() > MAXIMUM_CREDENTIAL_ID_BYTES
                || !(credential_id.starts_with("ssh-pubkey:")
                    || credential_id.starts_with("ssh-cert:"))
            {
                return Err(invalid("credential mapping ID is invalid"));
            }
            if credential_id == "ssh-pubkey:" || credential_id == "ssh-cert:" {
                return Err(invalid("credential mapping ID is invalid"));
            }
            let audit_subject = mapping
                .audit_subject
                .map(|value| validate_subject(value, "audit subject is invalid"))
                .transpose()?;
            let quota_subject = mapping
                .quota_subject
                .map(|value| validate_subject(value, "quota subject is invalid"))
                .transpose()?;
            if audit_subject.is_none() && quota_subject.is_none() {
                return Err(invalid("credential mapping is empty"));
            }
            Ok((
                credential_id,
                CredentialMapping {
                    audit_subject,
                    quota_subject,
                },
            ))
        })
        .collect()
}

fn validate_local_backend(raw: RawLocalBackendConfig) -> io::Result<LocalBackendConfig> {
    validate_backend_capacity(raw.maximum_concurrent_builds)?;
    Ok(LocalBackendConfig {
        target: BackendTarget::new(
            &raw.name,
            BackendKind::Local,
            &raw.system,
            &raw.supported_features,
        )?,
        maximum_concurrent_builds: raw.maximum_concurrent_builds,
    })
}

fn validate_static_ssh_backends(
    raw: Vec<RawStaticSshBackendConfig>,
) -> io::Result<Vec<StaticSshBackendConfig>> {
    if raw.len() > MAXIMUM_STATIC_SSH_BACKENDS {
        return Err(invalid("static SSH backend count exceeds limit"));
    }
    let mut backends = Vec::with_capacity(raw.len());
    for backend in raw {
        if backends
            .iter()
            .any(|existing: &StaticSshBackendConfig| existing.target.name() == backend.name)
        {
            return Err(invalid("static SSH backend name is ambiguous"));
        }
        validate_backend_capacity(backend.maximum_concurrent_builds)?;
        if !valid_ssh_destination(&backend.destination) {
            return Err(invalid("static SSH destination is invalid"));
        }
        validate_identity_file(&backend.identity_file)?;
        validate_known_hosts_file(&backend.known_hosts_file)?;
        let ssh_program = backend
            .ssh_program
            .unwrap_or_else(|| PathBuf::from(PACKAGED_SSH_PROGRAM.unwrap_or(SYSTEM_SSH_PROGRAM)));
        validate_executable_file(&ssh_program, "static SSH program is invalid")?;
        backends.push(StaticSshBackendConfig {
            target: BackendTarget::new(
                &backend.name,
                BackendKind::StaticSsh,
                &backend.system,
                &backend.supported_features,
            )?,
            maximum_concurrent_builds: backend.maximum_concurrent_builds,
            destination: backend.destination,
            identity_file: backend.identity_file,
            known_hosts_file: backend.known_hosts_file,
            ssh_program,
        });
    }
    Ok(backends)
}

fn validate_backend_capacity(maximum: usize) -> io::Result<()> {
    if maximum == 0 || maximum > MAXIMUM_BACKEND_CONCURRENT_BUILDS {
        return Err(invalid("backend concurrency limit is invalid"));
    }
    Ok(())
}

fn valid_ssh_destination(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAXIMUM_SSH_DESTINATION_BYTES
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'@' | b':' | b'[' | b']')
        })
}

fn validate_identity_file(path: &Path) -> io::Result<()> {
    let metadata = validate_regular_file(path, "static SSH identity file is invalid")?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(invalid("static SSH identity file permissions are unsafe"));
    }
    Ok(())
}

fn validate_known_hosts_file(path: &Path) -> io::Result<()> {
    let metadata = validate_regular_file(path, "static SSH known-hosts file is invalid")?;
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(invalid(
            "static SSH known-hosts file permissions are unsafe",
        ));
    }
    if metadata.size() == 0 || metadata.size() > 1024 * 1024 {
        return Err(invalid("static SSH known-hosts file is invalid"));
    }
    let contents =
        fs::read_to_string(path).map_err(|_| invalid("static SSH known-hosts file is invalid"))?;
    let pinned_key = contents.lines().any(|line| {
        let line = line.trim();
        !line.is_empty() && !line.starts_with('#') && line.split_ascii_whitespace().count() >= 3
    });
    if !pinned_key {
        return Err(invalid(
            "static SSH known-hosts file has no pinned host key",
        ));
    }
    Ok(())
}

fn validate_regular_file(path: &Path, message: &'static str) -> io::Result<fs::Metadata> {
    if !path.is_absolute() {
        return Err(invalid(message));
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| invalid(message))?;
    if !metadata.file_type().is_file() {
        return Err(invalid(message));
    }
    Ok(metadata)
}

fn validate_executable_file(path: &Path, message: &'static str) -> io::Result<()> {
    let metadata = fs::metadata(path).map_err(|_| invalid(message))?;
    if !path.is_absolute()
        || !metadata.file_type().is_file()
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err(invalid(message));
    }
    Ok(())
}

fn validate_subject(value: String, message: &'static str) -> io::Result<String> {
    if value.is_empty() || value.len() > MAXIMUM_SUBJECT_BYTES {
        return Err(invalid(message));
    }
    Ok(value)
}

fn read_secret(path: PathBuf) -> io::Result<String> {
    if !path.is_absolute() {
        return Err(invalid("database URL file path is invalid"));
    }
    let value =
        fs::read_to_string(path).map_err(|_| invalid("database URL file could not be read"))?;
    nonempty(value.trim().to_owned(), "database URL is invalid")
}

fn environment_string(name: &'static str) -> io::Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(invalid("environment override is invalid")),
    }
}

fn environment_path(name: &'static str) -> io::Result<Option<PathBuf>> {
    match std::env::var_os(name) {
        Some(value) => {
            let path = PathBuf::from(value);
            if path.as_os_str().is_empty() {
                return Err(invalid("environment path override is invalid"));
            }
            Ok(Some(path))
        }
        None => Ok(None),
    }
}

fn parse_retention(value: &str) -> io::Result<OutputRetention> {
    OutputRetention::parse(value)
}

fn parse_positive_u64(value: &str, message: &'static str) -> io::Result<u64> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid(message));
    }
    value.parse().map_err(|_| invalid(message))
}

fn parse_positive_usize(value: &str, message: &'static str) -> io::Result<usize> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid(message));
    }
    value.parse().map_err(|_| invalid(message))
}

fn nonempty(value: String, message: &'static str) -> io::Result<String> {
    if value.trim().is_empty() {
        return Err(invalid(message));
    }
    Ok(value)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
