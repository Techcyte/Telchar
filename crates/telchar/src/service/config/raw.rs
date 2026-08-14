use super::*;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawServiceConfig {
    pub(super) running_disconnect_policy: Option<String>,
    pub(super) output_retention_seconds: Option<u64>,
    pub(super) maximum_retained_input_bytes: Option<u64>,
    pub(super) database: Option<DatabaseSection>,
    pub(super) ipc: Option<IpcSection>,
    pub(super) nomad_callback: Option<RawNomadCallbackConfig>,
    pub(super) identity: Option<IdentityConfig>,
    pub(super) scheduling: Option<SchedulingConfig>,
    pub(super) backends: Option<BackendConfig>,
}

impl RawServiceConfig {
    pub(super) fn parse(raw: &str) -> io::Result<Self> {
        toml::from_str(raw).map_err(|_| invalid("service configuration is invalid"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DatabaseSection {
    pub(super) url_file: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IpcSection {
    pub(super) socket: Option<PathBuf>,
    pub(super) maximum_sessions: Option<usize>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadCallbackConfig {
    pub(super) bind: Option<String>,
    pub(super) public_url: Option<String>,
    pub(super) maximum_connections: Option<usize>,
    pub(super) maximum_header_bytes: Option<usize>,
    pub(super) maximum_body_bytes: Option<usize>,
    pub(super) authentication_request_timeout_seconds: Option<u64>,
    pub(super) shutdown_drain_timeout_seconds: Option<u64>,
    pub(super) maximum_jwks_bytes: Option<usize>,
    pub(super) maximum_retained_nonces: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IdentityConfig {
    #[serde(default)]
    pub(super) credentials: BTreeMap<String, RawCredentialMapping>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawIdentityConfig {
    #[serde(default)]
    pub(super) credentials: BTreeMap<String, RawCredentialMapping>,
}

impl RawIdentityConfig {
    pub(super) fn parse(raw: &str) -> io::Result<Self> {
        toml::from_str(raw).map_err(|_| invalid("identity mapping file is invalid"))
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SchedulingConfig {
    pub(super) default: Option<RawSchedulingLimits>,
    #[serde(default)]
    pub(super) subjects: BTreeMap<String, RawSchedulingLimits>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawSchedulingLimits {
    pub(super) maximum_queued_builds: usize,
    pub(super) maximum_active_builds: usize,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BackendConfig {
    pub(super) permit_wait_seconds: Option<u64>,
    pub(super) local: Option<RawLocalBackendConfig>,
    #[serde(default)]
    pub(super) static_ssh: Vec<RawStaticSshBackendConfig>,
    #[serde(default)]
    pub(super) nomad: Vec<RawNomadBackendConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawLocalBackendConfig {
    pub(super) name: String,
    pub(super) system: String,
    #[serde(default)]
    pub(super) supported_features: Vec<String>,
    pub(super) maximum_concurrent_builds: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawStaticSshBackendConfig {
    pub(super) name: String,
    pub(super) system: String,
    #[serde(default)]
    pub(super) supported_features: Vec<String>,
    pub(super) maximum_concurrent_builds: usize,
    pub(super) destination: String,
    pub(super) identity_file: PathBuf,
    pub(super) known_hosts_file: PathBuf,
    pub(super) ssh_program: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadBackendConfig {
    pub(super) name: String,
    pub(super) system: String,
    #[serde(default)]
    pub(super) supported_features: Vec<String>,
    pub(super) maximum_concurrent_builds: usize,
    pub(super) endpoint: String,
    pub(super) namespace: String,
    pub(super) token_file: Option<PathBuf>,
    pub(super) ca_certificate_file: Option<PathBuf>,
    pub(super) client_certificate_file: Option<PathBuf>,
    pub(super) client_key_file: Option<PathBuf>,
    pub(super) driver: String,
    #[serde(default)]
    pub(super) driver_config: toml::Table,
    pub(super) resources: RawNomadResources,
    pub(super) job_name_scope: String,
    pub(super) poll_interval_seconds: u64,
    pub(super) runtime_limit_seconds: u64,
    pub(super) transfer_endpoint: Option<String>,
    pub(super) transfer_authentication: RawNomadTransferAuthentication,
    pub(super) store: RawNomadStoreConfig,
    pub(super) transfer_limits: RawNomadTransferLimits,
    pub(super) prestart: Option<RawNomadPrestartConfig>,
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum RawNomadTransferAuthentication {
    WorkloadIdentity {
        issuer: String,
        jwks_url: String,
        audience: String,
        ca_certificate_file: Option<PathBuf>,
    },
    Hmac {
        key_id: String,
        secret_file: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum RawNomadStoreConfig {
    Daemon { uri: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadTransferLimits {
    pub(super) maximum_manifest_paths: usize,
    pub(super) maximum_manifest_bytes: u64,
    pub(super) maximum_input_nar_bytes: u64,
    pub(super) maximum_total_input_bytes: u64,
    pub(super) maximum_output_nar_bytes: u64,
    pub(super) maximum_total_output_bytes: u64,
    pub(super) maximum_frame_metadata_bytes: usize,
    pub(super) stream_buffer_bytes: usize,
    pub(super) maximum_live_log_chunk_bytes: usize,
    pub(super) live_log_queue_bytes: usize,
    pub(super) transfer_idle_timeout_seconds: u64,
    pub(super) setup_timeout_seconds: u64,
    pub(super) output_collection_timeout_seconds: u64,
    pub(super) maximum_connection_lifetime_seconds: u64,
    pub(super) authentication_lifetime_seconds: u64,
    pub(super) clock_skew_seconds: u64,
    pub(super) nonce_retention_seconds: u64,
    pub(super) reconnect_timeout_seconds: u64,
    pub(super) maximum_diagnostic_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadPrestartConfig {
    pub(super) driver: String,
    #[serde(default)]
    pub(super) driver_config: toml::Table,
    pub(super) resources: RawNomadResources,
    pub(super) timeout_seconds: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadResources {
    pub(super) cpu_mhz: u64,
    pub(super) memory_mb: u64,
    pub(super) disk_mb: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawCredentialMapping {
    pub(super) audit_subject: Option<String>,
    pub(super) quota_subject: Option<String>,
}
