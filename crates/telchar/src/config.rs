use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::deployment::{DeploymentConfig, OutputRetention, RunningDisconnectPolicy};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/telchar/telchar.toml";
const DEFAULT_MAXIMUM_IPC_SESSIONS: usize = 64;
const MAXIMUM_IPC_SESSIONS: usize = 65_536;
const MAXIMUM_CREDENTIAL_MAPPINGS: usize = 4_096;
const MAXIMUM_CREDENTIAL_ID_BYTES: usize = 1_024;
const MAXIMUM_SUBJECT_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialMapping {
    pub audit_subject: Option<String>,
    pub quota_subject: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    deployment: Option<DeploymentConfig>,
    running_disconnect_policy: RunningDisconnectPolicy,
    database_url: Option<String>,
    ipc_socket: Option<PathBuf>,
    maximum_ipc_sessions: usize,
    credential_mappings: BTreeMap<String, CredentialMapping>,
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
        Ok(Self {
            deployment,
            running_disconnect_policy,
            database_url,
            ipc_socket,
            maximum_ipc_sessions,
            credential_mappings,
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
