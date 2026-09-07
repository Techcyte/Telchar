use super::*;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawServiceConfig {
    pub(super) running_disconnect_policy: Option<String>,
    pub(super) output_retention_seconds: Option<u64>,
    pub(super) maximum_retained_input_bytes: Option<u64>,
    pub(super) cache_publication: Option<CachePublicationSection>,
    pub(super) database: Option<DatabaseSection>,
    pub(super) ipc: Option<IpcSection>,
    pub(super) identity: Option<IdentityConfig>,
    pub(super) scheduling: Option<SchedulingConfig>,
    pub(super) backends: Option<BackendConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CachePublicationSection {
    pub(super) executable: PathBuf,
    #[serde(default)]
    pub(super) arguments: Vec<String>,
    pub(super) timeout_seconds: Option<u64>,
    pub(super) maximum_input_bytes: Option<usize>,
}

impl RawServiceConfig {
    pub(super) fn parse(raw: &str) -> io::Result<Self> {
        toml::from_str(raw).map_err(|error: toml::de::Error| {
            if let Some(prefix) = error.span().and_then(|span| raw.get(..span.start)) {
                let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
                let column = prefix
                    .rsplit('\n')
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .count()
                    + 1;
                tracing::error!(
                    event = "configuration.parse_failed",
                    line,
                    column,
                    "service configuration parse failed"
                );
            } else {
                tracing::error!(
                    event = "configuration.parse_failed",
                    "service configuration parse failed"
                );
            }
            invalid("service configuration is invalid")
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DatabaseSection {
    pub(super) url_file: Option<PathBuf>,
    pub(super) ownership_renewal_seconds: Option<u64>,
    pub(super) ownership_lease_seconds: Option<u64>,
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
    pub(super) nomad_callback: Option<RawNomadCallbackConfig>,
    #[serde(default)]
    pub(super) ssh: Vec<RawSshConfig>,
    #[serde(default)]
    pub(super) nomad: Vec<RawNomadConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawLocalBackendConfig {
    pub(super) name: String,
    pub(super) system: String,
    #[serde(default)]
    pub(super) supported_features: Vec<String>,
    pub(super) maximum_concurrent_builds: usize,
    pub(super) selection_priority: Option<u32>,
}

#[derive(Default, Deserialize)]
pub(super) struct RawSshConfig {
    pub(super) system: Option<String>,
    pub(super) supported_features: Option<Vec<String>>,
    pub(super) maximum_concurrent_builds: Option<usize>,
    pub(super) selection_priority: Option<u32>,
    pub(super) ready_check_interval_seconds: Option<u64>,
    pub(super) unavailable_check_interval_seconds: Option<u64>,
    pub(super) check_timeout_seconds: Option<u64>,
    pub(super) ssh_user: Option<String>,
    pub(super) identity_file: Option<PathBuf>,
    pub(super) known_hosts_file: Option<PathBuf>,
    pub(super) ssh_program: Option<PathBuf>,
    #[serde(flatten)]
    pub(super) backends: BTreeMap<String, RawSshBackendConfig>,
}

#[derive(Deserialize)]
pub(super) struct RawSshBackendConfig {
    pub(super) source: String,
    pub(super) system: Option<String>,
    pub(super) supported_features: Option<Vec<String>>,
    pub(super) maximum_concurrent_builds: Option<usize>,
    pub(super) selection_priority: Option<u32>,
    pub(super) ready_check_interval_seconds: Option<u64>,
    pub(super) unavailable_check_interval_seconds: Option<u64>,
    pub(super) check_timeout_seconds: Option<u64>,
    pub(super) ssh_user: Option<String>,
    pub(super) identity_file: Option<PathBuf>,
    pub(super) known_hosts_file: Option<PathBuf>,
    pub(super) ssh_program: Option<PathBuf>,
    pub(super) endpoint: Option<String>,
    pub(super) service: Option<String>,
    pub(super) datacenter: Option<String>,
    pub(super) required_tags: Option<Vec<String>>,
    pub(super) passing_only: Option<bool>,
    pub(super) refresh_interval_seconds: Option<u64>,
    pub(super) request_timeout_seconds: Option<u64>,
    pub(super) token_file: Option<PathBuf>,
    pub(super) ca_certificate_file: Option<PathBuf>,
    #[serde(flatten)]
    pub(super) hosts: BTreeMap<String, RawSshHostConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawSshHostConfig {
    pub(super) address: String,
    pub(super) port: Option<u16>,
    pub(super) system: Option<String>,
    pub(super) supported_features: Option<Vec<String>>,
    pub(super) maximum_concurrent_builds: Option<usize>,
    pub(super) selection_priority: Option<u32>,
    pub(super) ready_check_interval_seconds: Option<u64>,
    pub(super) unavailable_check_interval_seconds: Option<u64>,
    pub(super) check_timeout_seconds: Option<u64>,
    pub(super) ssh_user: Option<String>,
    pub(super) identity_file: Option<PathBuf>,
    pub(super) known_hosts_file: Option<PathBuf>,
    pub(super) ssh_program: Option<PathBuf>,
}

#[derive(Clone, Default, Deserialize)]
pub(super) struct RawNomadConfig {
    pub(super) system: Option<String>,
    pub(super) supported_features: Option<Vec<String>>,
    pub(super) maximum_concurrent_builds: Option<usize>,
    pub(super) selection_priority: Option<u32>,
    pub(super) max_retries: Option<usize>,
    pub(super) endpoint: Option<String>,
    pub(super) namespace: Option<String>,
    pub(super) node_pool: Option<String>,
    pub(super) token_file: Option<PathBuf>,
    pub(super) ca_certificate_file: Option<PathBuf>,
    pub(super) client_certificate_file: Option<PathBuf>,
    pub(super) client_key_file: Option<PathBuf>,
    pub(super) driver: Option<String>,
    pub(super) driver_config: Option<toml::Table>,
    pub(super) resources: Option<RawNomadResources>,
    pub(super) priority: Option<RawNomadPriority>,
    pub(super) resource_profiles: Option<Vec<RawNomadResourceProfile>>,
    pub(super) job_name_scope: Option<String>,
    pub(super) poll_interval_seconds: Option<u64>,
    pub(super) runtime_limit_seconds: Option<u64>,
    pub(super) constraints: Option<Vec<RawNomadConstraint>>,
    pub(super) transfer_endpoint: Option<String>,
    pub(super) callback_connect: Option<RawNomadCallbackConnect>,
    pub(super) transfer_authentication: Option<RawNomadTransferAuthentication>,
    pub(super) store: Option<RawNomadStoreConfig>,
    pub(super) transfer_limits: Option<RawNomadTransferLimits>,
    pub(super) prestart: Option<RawNomadPrestartConfig>,
    #[serde(flatten)]
    pub(super) backends: BTreeMap<String, RawNomadBackendConfig>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadBackendConfig {
    pub(super) system: Option<String>,
    pub(super) supported_features: Option<Vec<String>>,
    pub(super) maximum_concurrent_builds: Option<usize>,
    pub(super) selection_priority: Option<u32>,
    pub(super) max_retries: Option<usize>,
    pub(super) endpoint: Option<String>,
    pub(super) namespace: Option<String>,
    pub(super) node_pool: Option<String>,
    pub(super) token_file: Option<PathBuf>,
    pub(super) ca_certificate_file: Option<PathBuf>,
    pub(super) client_certificate_file: Option<PathBuf>,
    pub(super) client_key_file: Option<PathBuf>,
    pub(super) driver: Option<String>,
    pub(super) driver_config: Option<toml::Table>,
    pub(super) resources: Option<RawNomadResources>,
    pub(super) priority: Option<RawNomadPriority>,
    pub(super) resource_profiles: Option<Vec<RawNomadResourceProfile>>,
    pub(super) job_name_scope: Option<String>,
    pub(super) poll_interval_seconds: Option<u64>,
    pub(super) runtime_limit_seconds: Option<u64>,
    pub(super) constraints: Option<Vec<RawNomadConstraint>>,
    pub(super) transfer_endpoint: Option<String>,
    pub(super) callback_connect: Option<RawNomadCallbackConnect>,
    pub(super) transfer_authentication: Option<RawNomadTransferAuthentication>,
    pub(super) store: Option<RawNomadStoreConfig>,
    pub(super) transfer_limits: Option<RawNomadTransferLimits>,
    pub(super) prestart: Option<RawNomadPrestartConfig>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadCallbackConnect {
    pub(super) source_service: String,
    pub(super) destination_service: String,
    pub(super) local_bind_port: u16,
    pub(super) sidecar_image: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadConstraint {
    pub(super) attribute: String,
    pub(super) operator: String,
    pub(super) value: String,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum RawNomadTransferAuthentication {
    WorkloadIdentity {
        issuer: Option<String>,
        #[serde(default)]
        verify_issuer: bool,
        jwks_url: String,
        audience: String,
        ca_certificate_file: Option<PathBuf>,
    },
    Hmac {
        key_id: String,
        secret_file: PathBuf,
    },
}

#[derive(Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum RawNomadStoreConfig {
    Daemon { uri: String },
}

#[derive(Clone, Deserialize)]
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

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadPrestartConfig {
    pub(super) driver: String,
    #[serde(default)]
    pub(super) driver_config: toml::Table,
    pub(super) resources: RawNomadResources,
    pub(super) timeout_seconds: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadResources {
    pub(super) cpu_mhz: u64,
    pub(super) memory_mb: u64,
    pub(super) disk_mb: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadPriority {
    pub(super) minimum: u8,
    pub(super) default: u8,
    pub(super) maximum: u8,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNomadResourceProfile {
    pub(super) name: String,
    pub(super) required_feature: String,
    pub(super) cpu_mhz: u64,
    pub(super) memory_mb: u64,
    pub(super) disk_mb: u64,
    pub(super) priority_minimum: u8,
    pub(super) priority_default: u8,
    pub(super) priority_maximum: u8,
    #[serde(default)]
    pub(super) constraints: Vec<RawNomadConstraint>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawCredentialMapping {
    pub(super) audit_subject: Option<String>,
    pub(super) quota_subject: Option<String>,
}
