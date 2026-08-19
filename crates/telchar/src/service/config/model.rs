use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NomadCallbackConfig {
    pub(super) bind: SocketAddr,
    pub(super) public_url: String,
    pub(super) maximum_connections: usize,
    pub(super) maximum_header_bytes: usize,
    pub(super) maximum_body_bytes: usize,
    pub(super) authentication_request_timeout: Duration,
    pub(super) shutdown_drain_timeout: Duration,
    pub(super) maximum_jwks_bytes: usize,
    pub(super) maximum_retained_nonces: usize,
}

impl NomadCallbackConfig {
    pub fn bind(&self) -> SocketAddr {
        self.bind
    }

    pub fn public_url(&self) -> &str {
        &self.public_url
    }

    pub fn maximum_connections(&self) -> usize {
        self.maximum_connections
    }

    pub fn maximum_header_bytes(&self) -> usize {
        self.maximum_header_bytes
    }

    pub fn maximum_body_bytes(&self) -> usize {
        self.maximum_body_bytes
    }

    pub fn authentication_request_timeout(&self) -> Duration {
        self.authentication_request_timeout
    }

    pub fn shutdown_drain_timeout(&self) -> Duration {
        self.shutdown_drain_timeout
    }

    pub fn maximum_jwks_bytes(&self) -> usize {
        self.maximum_jwks_bytes
    }

    pub fn maximum_retained_nonces(&self) -> usize {
        self.maximum_retained_nonces
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialMapping {
    pub audit_subject: Option<String>,
    pub quota_subject: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulingLimits {
    pub(super) maximum_queued_builds: usize,
    pub(super) maximum_active_builds: usize,
}

impl SchedulingLimits {
    pub fn new(maximum_queued_builds: usize, maximum_active_builds: usize) -> io::Result<Self> {
        if maximum_queued_builds == 0
            || maximum_queued_builds > MAXIMUM_SCHEDULING_BUILDS
            || maximum_active_builds == 0
            || maximum_active_builds > MAXIMUM_SCHEDULING_BUILDS
        {
            return Err(invalid("scheduling limits are invalid"));
        }
        Ok(Self {
            maximum_queued_builds,
            maximum_active_builds,
        })
    }

    pub fn maximum_queued_builds(self) -> usize {
        self.maximum_queued_builds
    }

    pub fn maximum_active_builds(self) -> usize {
        self.maximum_active_builds
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalBackendConfig {
    pub(super) target: BackendTarget,
    pub(super) maximum_concurrent_builds: usize,
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
    pub(super) target: BackendTarget,
    pub(super) maximum_concurrent_builds: usize,
    pub(super) ready_check_interval: Duration,
    pub(super) unavailable_check_interval: Duration,
    pub(super) check_timeout: Duration,
    pub(super) destination: String,
    pub(super) identity_file: PathBuf,
    pub(super) known_hosts_file: PathBuf,
    pub(super) ssh_program: PathBuf,
}

impl StaticSshBackendConfig {
    pub fn target(&self) -> &BackendTarget {
        &self.target
    }

    pub fn maximum_concurrent_builds(&self) -> usize {
        self.maximum_concurrent_builds
    }

    pub fn ready_check_interval(&self) -> Duration {
        self.ready_check_interval
    }

    pub fn unavailable_check_interval(&self) -> Duration {
        self.unavailable_check_interval
    }

    pub fn check_timeout(&self) -> Duration {
        self.check_timeout
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadResources {
    pub(super) cpu_mhz: u64,
    pub(super) memory_mb: u64,
    pub(super) disk_mb: u64,
}

impl NomadResources {
    pub fn cpu_mhz(self) -> u64 {
        self.cpu_mhz
    }

    pub fn memory_mb(self) -> u64 {
        self.memory_mb
    }

    pub fn disk_mb(self) -> u64 {
        self.disk_mb
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NomadTransferAuthentication {
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

impl NomadTransferAuthentication {
    pub fn mode(&self) -> &'static str {
        match self {
            Self::WorkloadIdentity { .. } => "workload-identity",
            Self::Hmac { .. } => "hmac",
        }
    }

    pub fn issuer(&self) -> Option<&str> {
        match self {
            Self::WorkloadIdentity { issuer, .. } => Some(issuer),
            Self::Hmac { .. } => None,
        }
    }

    pub fn jwks_url(&self) -> Option<&str> {
        match self {
            Self::WorkloadIdentity { jwks_url, .. } => Some(jwks_url),
            Self::Hmac { .. } => None,
        }
    }

    pub fn audience(&self) -> Option<&str> {
        match self {
            Self::WorkloadIdentity { audience, .. } => Some(audience),
            Self::Hmac { .. } => None,
        }
    }

    pub fn ca_certificate_file(&self) -> Option<&Path> {
        match self {
            Self::WorkloadIdentity {
                ca_certificate_file,
                ..
            } => ca_certificate_file.as_deref(),
            Self::Hmac { .. } => None,
        }
    }

    pub fn key_id(&self) -> Option<&str> {
        match self {
            Self::WorkloadIdentity { .. } => None,
            Self::Hmac { key_id, .. } => Some(key_id),
        }
    }

    pub fn secret_file(&self) -> Option<&Path> {
        match self {
            Self::WorkloadIdentity { .. } => None,
            Self::Hmac { secret_file, .. } => Some(secret_file),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NomadStoreConfig {
    pub(super) uri: String,
}

impl NomadStoreConfig {
    pub fn mode(&self) -> &'static str {
        "daemon"
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadTransferLimits {
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
    pub(super) transfer_idle_timeout: Duration,
    pub(super) setup_timeout: Duration,
    pub(super) output_collection_timeout: Duration,
    pub(super) maximum_connection_lifetime: Duration,
    pub(super) authentication_lifetime: Duration,
    pub(super) clock_skew: Duration,
    pub(super) nonce_retention: Duration,
    pub(super) reconnect_timeout: Duration,
    pub(super) maximum_diagnostic_bytes: usize,
}

impl NomadTransferLimits {
    pub fn maximum_manifest_paths(self) -> usize {
        self.maximum_manifest_paths
    }

    pub fn maximum_manifest_bytes(self) -> u64 {
        self.maximum_manifest_bytes
    }

    pub fn maximum_input_nar_bytes(self) -> u64 {
        self.maximum_input_nar_bytes
    }

    pub fn maximum_total_input_bytes(self) -> u64 {
        self.maximum_total_input_bytes
    }

    pub fn maximum_output_nar_bytes(self) -> u64 {
        self.maximum_output_nar_bytes
    }

    pub fn maximum_total_output_bytes(self) -> u64 {
        self.maximum_total_output_bytes
    }

    pub fn maximum_frame_metadata_bytes(self) -> usize {
        self.maximum_frame_metadata_bytes
    }

    pub fn stream_buffer_bytes(self) -> usize {
        self.stream_buffer_bytes
    }

    pub fn maximum_live_log_chunk_bytes(self) -> usize {
        self.maximum_live_log_chunk_bytes
    }

    pub fn live_log_queue_bytes(self) -> usize {
        self.live_log_queue_bytes
    }

    pub fn transfer_idle_timeout(self) -> Duration {
        self.transfer_idle_timeout
    }

    pub fn setup_timeout(self) -> Duration {
        self.setup_timeout
    }

    pub fn output_collection_timeout(self) -> Duration {
        self.output_collection_timeout
    }

    pub fn maximum_connection_lifetime(self) -> Duration {
        self.maximum_connection_lifetime
    }

    pub fn authentication_lifetime(self) -> Duration {
        self.authentication_lifetime
    }

    pub fn clock_skew(self) -> Duration {
        self.clock_skew
    }

    pub fn nonce_retention(self) -> Duration {
        self.nonce_retention
    }

    pub fn reconnect_timeout(self) -> Duration {
        self.reconnect_timeout
    }

    pub fn maximum_diagnostic_bytes(self) -> usize {
        self.maximum_diagnostic_bytes
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NomadPrestartConfig {
    pub(super) driver: String,
    pub(super) driver_config: serde_json::Map<String, serde_json::Value>,
    pub(super) resources: NomadResources,
    pub(super) timeout: Duration,
}

impl NomadPrestartConfig {
    pub fn driver(&self) -> &str {
        &self.driver
    }

    pub fn driver_config(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.driver_config
    }

    pub fn resources(&self) -> NomadResources {
        self.resources
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NomadBackendConfig {
    pub(super) target: BackendTarget,
    pub(super) maximum_concurrent_builds: usize,
    pub(super) endpoint: String,
    pub(super) namespace: String,
    pub(super) token_file: Option<PathBuf>,
    pub(super) ca_certificate_file: Option<PathBuf>,
    pub(super) client_certificate_file: Option<PathBuf>,
    pub(super) client_key_file: Option<PathBuf>,
    pub(super) driver: String,
    pub(super) driver_config: serde_json::Map<String, serde_json::Value>,
    pub(super) resources: NomadResources,
    pub(super) job_name_scope: String,
    pub(super) poll_interval: Duration,
    pub(super) runtime_limit: Duration,
    pub(super) transfer_endpoint: String,
    pub(super) transfer_authentication: NomadTransferAuthentication,
    pub(super) store: NomadStoreConfig,
    pub(super) transfer_limits: NomadTransferLimits,
    pub(super) prestart: Option<NomadPrestartConfig>,
}

impl NomadBackendConfig {
    pub fn target(&self) -> &BackendTarget {
        &self.target
    }

    pub fn maximum_concurrent_builds(&self) -> usize {
        self.maximum_concurrent_builds
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn token_file(&self) -> Option<&Path> {
        self.token_file.as_deref()
    }

    pub fn ca_certificate_file(&self) -> Option<&Path> {
        self.ca_certificate_file.as_deref()
    }

    pub fn client_certificate_file(&self) -> Option<&Path> {
        self.client_certificate_file.as_deref()
    }

    pub fn client_key_file(&self) -> Option<&Path> {
        self.client_key_file.as_deref()
    }

    pub fn driver(&self) -> &str {
        &self.driver
    }

    pub fn driver_config(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.driver_config
    }

    pub fn resources(&self) -> NomadResources {
        self.resources
    }

    pub fn job_name_scope(&self) -> &str {
        &self.job_name_scope
    }

    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    pub fn runtime_limit(&self) -> Duration {
        self.runtime_limit
    }

    pub fn transfer_endpoint(&self) -> &str {
        &self.transfer_endpoint
    }

    pub fn transfer_authentication(&self) -> &NomadTransferAuthentication {
        &self.transfer_authentication
    }

    pub fn store(&self) -> &NomadStoreConfig {
        &self.store
    }

    pub fn transfer_limits(&self) -> NomadTransferLimits {
        self.transfer_limits
    }

    pub fn prestart(&self) -> Option<&NomadPrestartConfig> {
        self.prestart.as_ref()
    }
}
