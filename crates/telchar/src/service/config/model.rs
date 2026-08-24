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
    pub(super) port: u16,
    pub(super) identity_file: PathBuf,
    pub(super) known_hosts_file: PathBuf,
    pub(super) ssh_program: PathBuf,
}

impl StaticSshBackendConfig {
    pub(crate) fn discovered(
        target: BackendTarget,
        maximum_concurrent_builds: usize,
        destination: String,
        port: u16,
        identity_file: PathBuf,
        known_hosts_file: PathBuf,
        ssh_program: PathBuf,
    ) -> Self {
        Self {
            target,
            maximum_concurrent_builds,
            ready_check_interval: Duration::from_secs(300),
            unavailable_check_interval: Duration::from_secs(60),
            check_timeout: Duration::from_secs(10),
            destination,
            port,
            identity_file,
            known_hosts_file,
            ssh_program,
        }
    }

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

    pub fn port(&self) -> u16 {
        self.port
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaticSshConsulConfig {
    pub(super) name: String,
    pub(super) system: String,
    pub(super) supported_features: Vec<String>,
    pub(super) maximum_concurrent_builds_per_instance: usize,
    pub(super) endpoint: String,
    pub(super) service: String,
    pub(super) datacenter: Option<String>,
    pub(super) required_tags: Vec<String>,
    pub(super) passing_only: bool,
    pub(super) refresh_interval: Duration,
    pub(super) request_timeout: Duration,
    pub(super) token_file: Option<PathBuf>,
    pub(super) ca_certificate_file: Option<PathBuf>,
    pub(super) ssh_user: String,
    pub(super) identity_file: PathBuf,
    pub(super) known_hosts_file: PathBuf,
    pub(super) ssh_program: PathBuf,
}

impl StaticSshConsulConfig {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn system(&self) -> &str {
        &self.system
    }
    pub fn supported_features(&self) -> &[String] {
        &self.supported_features
    }
    pub fn maximum_concurrent_builds_per_instance(&self) -> usize {
        self.maximum_concurrent_builds_per_instance
    }
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn service(&self) -> &str {
        &self.service
    }
    pub fn datacenter(&self) -> Option<&str> {
        self.datacenter.as_deref()
    }
    pub fn required_tags(&self) -> &[String] {
        &self.required_tags
    }
    pub fn passing_only(&self) -> bool {
        self.passing_only
    }
    pub fn refresh_interval(&self) -> Duration {
        self.refresh_interval
    }
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }
    pub fn token_file(&self) -> Option<&Path> {
        self.token_file.as_deref()
    }
    pub fn ca_certificate_file(&self) -> Option<&Path> {
        self.ca_certificate_file.as_deref()
    }
    pub fn ssh_user(&self) -> &str {
        &self.ssh_user
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadPriority {
    pub(super) minimum: u8,
    pub(super) default: u8,
    pub(super) maximum: u8,
}

impl NomadPriority {
    pub fn minimum(self) -> u8 {
        self.minimum
    }

    pub fn default(self) -> u8 {
        self.default
    }

    pub fn maximum(self) -> u8 {
        self.maximum
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NomadResourceProfile {
    pub(super) name: String,
    pub(super) required_feature: String,
    pub(super) resources: NomadResources,
    pub(super) priority: NomadPriority,
    pub(super) constraints: Vec<NomadConstraint>,
}

impl NomadResourceProfile {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn required_feature(&self) -> &str {
        &self.required_feature
    }

    pub fn resources(&self) -> NomadResources {
        self.resources
    }

    pub fn priority(&self) -> NomadPriority {
        self.priority
    }

    pub fn constraints(&self) -> &[NomadConstraint] {
        &self.constraints
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedNomadResourceProfile<'a> {
    name: &'a str,
    resources: NomadResources,
    priority: NomadPriority,
    constraints: &'a [NomadConstraint],
}

impl<'a> SelectedNomadResourceProfile<'a> {
    pub fn name(&self) -> &'a str {
        self.name
    }

    pub fn resources(&self) -> NomadResources {
        self.resources
    }

    pub fn priority(&self) -> NomadPriority {
        self.priority
    }

    pub fn constraints(&self) -> &'a [NomadConstraint] {
        self.constraints
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NomadResourceProfileSelectionError;

impl std::fmt::Display for NomadResourceProfileSelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Nomad resource profile selection is ambiguous")
    }
}

impl std::error::Error for NomadResourceProfileSelectionError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NomadTransferAuthentication {
    WorkloadIdentity {
        issuer: Option<String>,
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

impl NomadTransferAuthentication {
    pub fn mode(&self) -> &'static str {
        match self {
            Self::WorkloadIdentity { .. } => "workload-identity",
            Self::Hmac { .. } => "hmac",
        }
    }

    pub fn issuer(&self) -> Option<&str> {
        match self {
            Self::WorkloadIdentity { issuer, .. } => issuer.as_deref(),
            Self::Hmac { .. } => None,
        }
    }

    pub fn verify_issuer(&self) -> bool {
        match self {
            Self::WorkloadIdentity { verify_issuer, .. } => *verify_issuer,
            Self::Hmac { .. } => false,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NomadConstraint {
    pub(super) attribute: String,
    pub(super) operator: String,
    pub(super) value: String,
}

impl NomadConstraint {
    pub fn attribute(&self) -> &str {
        &self.attribute
    }

    pub fn operator(&self) -> &str {
        &self.operator
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NomadCallbackConnect {
    pub(super) source_service: String,
    pub(super) destination_service: String,
    pub(super) local_bind_port: u16,
}

impl NomadCallbackConnect {
    pub fn source_service(&self) -> &str {
        &self.source_service
    }

    pub fn destination_service(&self) -> &str {
        &self.destination_service
    }

    pub fn local_bind_port(&self) -> u16 {
        self.local_bind_port
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NomadBackendConfig {
    pub(super) target: BackendTarget,
    pub(super) maximum_concurrent_builds: usize,
    pub(super) max_retries: usize,
    pub(super) endpoint: String,
    pub(super) namespace: String,
    pub(super) node_pool: String,
    pub(super) token_file: Option<PathBuf>,
    pub(super) ca_certificate_file: Option<PathBuf>,
    pub(super) client_certificate_file: Option<PathBuf>,
    pub(super) client_key_file: Option<PathBuf>,
    pub(super) driver: String,
    pub(super) driver_config: serde_json::Map<String, serde_json::Value>,
    pub(super) resources: NomadResources,
    pub(super) priority: NomadPriority,
    pub(super) resource_profiles: Vec<NomadResourceProfile>,
    pub(super) job_name_scope: String,
    pub(super) poll_interval: Duration,
    pub(super) runtime_limit: Duration,
    pub(super) constraints: Vec<NomadConstraint>,
    pub(super) transfer_endpoint: String,
    pub(super) callback_connect: Option<NomadCallbackConnect>,
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

    pub fn max_retries(&self) -> usize {
        self.max_retries
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn node_pool(&self) -> &str {
        &self.node_pool
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

    pub fn priority(&self) -> NomadPriority {
        self.priority
    }

    pub fn resource_profiles(&self) -> &[NomadResourceProfile] {
        &self.resource_profiles
    }

    pub fn select_resource_profile<'a, S: AsRef<str>>(
        &'a self,
        required_features: &[S],
    ) -> Result<SelectedNomadResourceProfile<'a>, NomadResourceProfileSelectionError> {
        let mut selected = None;
        for profile in &self.resource_profiles {
            let matched = required_features
                .iter()
                .filter(|feature| feature.as_ref() == profile.required_feature())
                .count();
            if matched > 1 || (matched == 1 && selected.is_some()) {
                return Err(NomadResourceProfileSelectionError);
            }
            if matched == 1 {
                selected = Some(profile);
            }
        }
        Ok(match selected {
            Some(profile) => SelectedNomadResourceProfile {
                name: profile.name(),
                resources: profile.resources(),
                priority: profile.priority(),
                constraints: profile.constraints(),
            },
            None => SelectedNomadResourceProfile {
                name: "default",
                resources: self.resources,
                priority: self.priority,
                constraints: &[],
            },
        })
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

    pub fn constraints(&self) -> &[NomadConstraint] {
        &self.constraints
    }

    pub fn transfer_endpoint(&self) -> &str {
        &self.transfer_endpoint
    }

    pub fn callback_connect(&self) -> Option<&NomadCallbackConnect> {
        self.callback_connect.as_ref()
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
