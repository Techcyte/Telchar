use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Deployment {
    name: String,
    system: String,
    account_id: String,
    region: String,
    image_arn: String,
    image_version: String,
    maximum_concurrent_builds: usize,
    maximum_duration_seconds: u64,
    execution_timeout_seconds: u64,
    admission_timeout_seconds: u64,
    provider_timeout_seconds: u64,
    cleanup_interval_seconds: u64,
    callback_url: String,
    callback_address: SocketAddr,
    staging_directory: PathBuf,
    maximum_paths: usize,
    maximum_nar_bytes: u64,
    maximum_total_bytes: u64,
    metadata_bytes: usize,
    chunk_bytes: usize,
}

/// Validated deployment policy; parsing does not attest AWS credentials or image contents.
#[derive(Clone, Debug, PartialEq)]
pub struct MicrovmBackendConfig(Deployment);

impl MicrovmBackendConfig {
    pub fn parse(raw: &str) -> io::Result<Self> {
        let deployment: Deployment = toml::from_str(raw).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid MicroVM configuration")
        })?;
        let d = &deployment;
        let component = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        };
        let image_prefix = format!(
            "arn:aws:lambda:{}:{}:microvm-image:",
            d.region, d.account_id
        );
        let image_name = d.image_arn.strip_prefix(&image_prefix);
        if !component(&d.name)
            || d.system != "aarch64-linux"
            || d.account_id.len() != 12
            || !d.account_id.bytes().all(|b| b.is_ascii_digit())
            || !component(&d.region)
            || image_name.is_none_or(|name| !component(name))
            || d.image_version
                .parse::<u64>()
                .ok()
                .is_none_or(|v| v == 0 || v.to_string() != d.image_version)
            || !(1..=65536).contains(&d.maximum_concurrent_builds)
            || !(60..=28800).contains(&d.maximum_duration_seconds)
            || d.execution_timeout_seconds == 0
            || d.execution_timeout_seconds >= d.maximum_duration_seconds
            || d.admission_timeout_seconds == 0
            || d.admission_timeout_seconds >= d.execution_timeout_seconds
            || !(1..=60).contains(&d.provider_timeout_seconds)
            || d.provider_timeout_seconds >= d.admission_timeout_seconds
            || !(1..=300).contains(&d.cleanup_interval_seconds)
            || !d.staging_directory.is_absolute()
            || d.staging_directory
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            || !(1..=1_000_000).contains(&d.maximum_paths)
            || d.maximum_nar_bytes == 0
            || d.maximum_total_bytes < d.maximum_nar_bytes
            || d.maximum_total_bytes > i64::MAX as u64
            || !(1..=8 * 1024 * 1024).contains(&d.metadata_bytes)
            || !(1..=256 * 1024).contains(&d.chunk_bytes)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid MicroVM deployment bounds or identity",
            ));
        }
        let url = url::Url::parse(&d.callback_url).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid MicroVM callback URL")
        })?;
        if url.scheme() != "wss"
            || url.host_str().is_none()
            || url.path() != "/microvm"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.port_or_known_default() == Some(0)
            || d.callback_address.port() == 0
            || d.callback_address.ip().is_unspecified()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid MicroVM callback address",
            ));
        }
        Ok(Self(deployment))
    }

    pub async fn provider(&self) -> io::Result<crate::microvm::Provider> {
        let timeout = Duration::from_secs(self.0.provider_timeout_seconds);
        let shared = tokio::time::timeout(
            timeout,
            aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(aws_config::Region::new(self.0.region.clone()))
                .load(),
        )
        .await
        .map_err(|_| io::Error::other("MicroVM credential configuration timed out"))?;
        let config = aws_sdk_lambdamicrovms::config::Builder::from(&shared)
            .endpoint_url(format!("https://lambda.{}.amazonaws.com", self.0.region))
            .build();
        let identity = aws_sdk_sts::config::Builder::from(&shared)
            .endpoint_url(format!("https://sts.{}.amazonaws.com", self.0.region))
            .build();
        crate::microvm::Provider::verify_account(
            config,
            identity,
            shared
                .credentials_provider()
                .ok_or_else(|| io::Error::other("MicroVM credentials unavailable"))?,
            &self.0.account_id,
            timeout,
        )
        .await
        .map_err(io::Error::other)
    }

    pub fn name(&self) -> &str {
        &self.0.name
    }
    pub fn maximum_concurrent_builds(&self) -> usize {
        self.0.maximum_concurrent_builds
    }
    pub fn admission_timeout(&self) -> Duration {
        Duration::from_secs(self.0.admission_timeout_seconds)
    }
    pub fn cleanup_interval(&self) -> Duration {
        Duration::from_secs(self.0.cleanup_interval_seconds)
    }
    pub fn execution_timeout(&self) -> Duration {
        Duration::from_secs(self.0.execution_timeout_seconds)
    }
    pub fn transfer_limits(&self) -> crate::microvm::session::Limits {
        crate::microvm::session::Limits {
            maximum_paths: self.0.maximum_paths,
            maximum_nar_bytes: self.0.maximum_nar_bytes,
            maximum_total_bytes: self.0.maximum_total_bytes,
            metadata_bytes: self.0.metadata_bytes,
            chunk_bytes: self.0.chunk_bytes,
        }
    }
    pub fn staging_directory(&self) -> &Path {
        &self.0.staging_directory
    }

    pub(crate) fn launch(
        &self,
        attempt_id: i64,
    ) -> io::Result<(crate::persistence::MicrovmLaunchIntent, String)> {
        let capability = crate::microvm::Capability::generate().map_err(io::Error::other)?;
        let identity = crate::microvm::Capability::generate().map_err(io::Error::other)?;
        let intent_id = identity.bearer().to_owned();
        let lifetime = self.0.maximum_duration_seconds as i32;
        let payload = capability
            .launch_payload(
                &self.0.callback_url,
                self.0.callback_address,
                &intent_id,
                lifetime,
            )
            .map_err(io::Error::other)?;
        let intent = crate::persistence::MicrovmLaunchIntent {
            intent_id: intent_id.clone(),
            attempt_id,
            backend_name: self.0.name.clone(),
            account_id: self.0.account_id.clone(),
            region: self.0.region.clone(),
            image_arn: self.0.image_arn.clone(),
            image_version: self.0.image_version.clone(),
            configuration_digest: self.fingerprint(),
            request_digest: crate::microvm::launch_request_digest(
                &self.0.image_arn,
                &self.0.image_version,
                &intent_id,
                &payload,
                lifetime,
            ),
            client_token: intent_id,
            credential_verifier: capability.verifier(),
            admission_cutoff: std::time::SystemTime::now()
                .checked_add(Duration::from_secs(self.0.admission_timeout_seconds))
                .ok_or_else(|| io::Error::other("MicroVM admission cutoff overflow"))?,
            maximum_duration_seconds: lifetime,
        };
        Ok((intent, payload))
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"telchar-microvm-deployment-v1\0");
        digest.update(serde_json::to_vec(&self.0).expect("deployment fields serialize"));
        digest.finalize().into()
    }
}
