//! AWS control-plane operations for exact MicroVM executions.

pub mod callback_service;
pub mod execution;
pub mod outputs;
pub mod rendezvous;
pub mod service;
pub mod session;

use std::time::Duration;

use aws_sdk_lambdamicrovms::{Client, Config};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

pub struct Capability {
    bearer: String,
    verifier: [u8; 32],
}

impl Capability {
    pub fn generate() -> Result<Self, ProviderError> {
        use std::io::Read;

        let mut entropy = [0; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut entropy))
            .map_err(|_| ProviderError)?;
        Ok(Self {
            bearer: URL_SAFE_NO_PAD.encode(entropy),
            verifier: capability_verifier(&entropy),
        })
    }

    pub fn bearer(&self) -> &str {
        &self.bearer
    }

    pub fn verifier(&self) -> [u8; 32] {
        self.verifier
    }

    pub fn launch_payload(
        &self,
        endpoint: &str,
        address: std::net::SocketAddr,
        intent_id: &str,
        lifetime_seconds: i32,
    ) -> Result<String, ProviderError> {
        let url = url::Url::parse(endpoint).map_err(|_| ProviderError)?;
        if url.scheme() != "wss"
            || url.host_str().is_none()
            || url.path() != "/microvm"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.port_or_known_default() == Some(0)
            || address.port() == 0
            || address.ip().is_unspecified()
            || intent_id.is_empty()
            || intent_id.len() > 128
            || !(1..=28800).contains(&lifetime_seconds)
        {
            return Err(ProviderError);
        }
        let payload = serde_json::to_string(&serde_json::json!({
            "version": 1,
            "endpoint": endpoint,
            "address": address.to_string(),
            "intent_id": intent_id,
            "bearer": self.bearer,
            "lifetime_seconds": lifetime_seconds,
        }))
        .map_err(|_| ProviderError)?;
        if payload.len() > 16384 {
            return Err(ProviderError);
        }
        Ok(payload)
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchPayload {
    version: u8,
    endpoint: String,
    address: std::net::SocketAddr,
    intent_id: String,
    bearer: String,
    lifetime_seconds: i32,
}

fn validate_launch_payload(
    payload: &str,
    intent: &crate::persistence::MicrovmLaunchIntent,
) -> Result<(), ProviderError> {
    if payload.len() > 16384 {
        return Err(ProviderError);
    }
    let hook: LaunchPayload = serde_json::from_str(payload).map_err(|_| ProviderError)?;
    let verifier = bearer_verifier(&hook.bearer).ok_or(ProviderError)?;
    if hook.version != 1
        || hook.intent_id != intent.intent_id
        || hook.lifetime_seconds != intent.maximum_duration_seconds
        || verifier != intent.credential_verifier
    {
        return Err(ProviderError);
    }
    Capability {
        bearer: hook.bearer,
        verifier,
    }
    .launch_payload(
        &hook.endpoint,
        hook.address,
        &hook.intent_id,
        hook.lifetime_seconds,
    )?;
    Ok(())
}

fn capability_verifier(entropy: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"telchar-microvm-capability-v1\0");
    digest.update(entropy);
    digest.finalize().into()
}

pub fn admit_session(
    owner: &crate::service::singleton_ownership::SingletonOwnership,
    intent_id: &str,
    provider_id: &str,
    bearer: &str,
) -> Result<bool, ProviderError> {
    owner.verify().map_err(|_| ProviderError)?;
    let Some(verifier) = bearer_verifier(bearer) else {
        return Ok(false);
    };
    crate::persistence::admit_microvm_session(owner.database(), intent_id, provider_id, &verifier)
        .map_err(|_| ProviderError)
}

fn bearer_verifier(bearer: &str) -> Option<[u8; 32]> {
    if bearer.len() != 43 {
        return None;
    }
    let entropy = URL_SAFE_NO_PAD.decode(bearer).ok()?;
    if entropy.len() != 32 || URL_SAFE_NO_PAD.encode(&entropy) != bearer {
        return None;
    }
    Some(capability_verifier(&entropy))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderError;

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MicroVM provider operation failed")
    }
}

impl std::error::Error for ProviderError {}

pub fn launch_request_digest(
    image: &str,
    version: &str,
    token: &str,
    payload: &str,
    lifetime_seconds: i32,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"telchar-microvm-launch-v1\0");
    for field in [image, version, token, payload] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    digest.update(lifetime_seconds.to_be_bytes());
    digest.finalize().into()
}

#[derive(Clone)]
pub struct Provider {
    client: Client,
    account_id: Option<String>,
    authority: Option<std::sync::Arc<ProviderAuthority>>,
}

struct ProviderAuthority {
    source: aws_credential_types::provider::SharedCredentialsProvider,
    identity: aws_sdk_sts::Config,
    timeout: Duration,
    attested: std::sync::Mutex<(aws_credential_types::Credentials, Client)>,
}

impl Provider {
    pub async fn verify_account(
        config: Config,
        identity: aws_sdk_sts::Config,
        credentials: aws_credential_types::provider::SharedCredentialsProvider,
        account_id: &str,
        timeout: Duration,
    ) -> Result<Self, ProviderError> {
        use aws_credential_types::provider::ProvideCredentials;

        if account_id.len() != 12 || !account_id.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ProviderError);
        }
        let provider = Self::new(config, timeout)?;
        let source = credentials;
        let credentials = tokio::time::timeout(timeout, source.provide_credentials())
            .await
            .map_err(|_| ProviderError)?
            .map_err(|_| ProviderError)?;
        let identity_config = identity.clone();
        let identity = aws_sdk_sts::Client::from_conf(
            identity
                .to_builder()
                .credentials_provider(credentials.clone())
                .retry_config(aws_sdk_sts::config::retry::RetryConfig::disabled())
                .timeout_config(
                    aws_sdk_sts::config::timeout::TimeoutConfig::builder()
                        .operation_timeout(timeout)
                        .operation_attempt_timeout(timeout)
                        .connect_timeout(timeout)
                        .read_timeout(timeout)
                        .build(),
                )
                .build(),
        );
        let result = identity
            .get_caller_identity()
            .send()
            .await
            .map_err(|_| ProviderError)?;
        if result.account() != Some(account_id) {
            return Err(ProviderError);
        }
        let config = provider
            .client
            .config()
            .to_builder()
            .credentials_provider(credentials.clone())
            .build();
        let mut provider = Self::new(config, timeout)?;
        provider.account_id = Some(account_id.to_owned());
        provider.authority = Some(std::sync::Arc::new(ProviderAuthority {
            source,
            identity: identity_config,
            timeout,
            attested: std::sync::Mutex::new((credentials, provider.client.clone())),
        }));
        Ok(provider)
    }

    pub fn new(config: Config, timeout: Duration) -> Result<Self, ProviderError> {
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(ProviderError);
        }
        let config = config
            .to_builder()
            .retry_config(aws_sdk_lambdamicrovms::config::retry::RetryConfig::disabled())
            .timeout_config(
                aws_sdk_lambdamicrovms::config::timeout::TimeoutConfig::builder()
                    .operation_timeout(timeout)
                    .operation_attempt_timeout(timeout)
                    .connect_timeout(timeout)
                    .read_timeout(timeout)
                    .build(),
            )
            .build();
        Ok(Self {
            client: Client::from_conf(config),
            account_id: None,
            authority: None,
        })
    }

    async fn authorized_client(&self) -> Result<Client, ProviderError> {
        use aws_credential_types::provider::ProvideCredentials;

        let Some(authority) = &self.authority else {
            return Ok(self.client.clone());
        };
        tokio::time::timeout(authority.timeout, async {
            let credentials = authority
                .source
                .provide_credentials()
                .await
                .map_err(|_| ProviderError)?;
            if credentials
                .expiry()
                .is_some_and(|expiry| expiry <= std::time::SystemTime::now())
            {
                return Err(ProviderError);
            }
            {
                let attested = authority.attested.lock().map_err(|_| ProviderError)?;
                if attested.0 == credentials {
                    return Ok(attested.1.clone());
                }
            }
            let provider = Self::verify_account(
                self.client.config().clone(),
                authority.identity.clone(),
                aws_credential_types::provider::SharedCredentialsProvider::new(credentials.clone()),
                self.account_id.as_deref().ok_or(ProviderError)?,
                authority.timeout,
            )
            .await?;
            let client = provider.client;
            *authority.attested.lock().map_err(|_| ProviderError)? = (credentials, client.clone());
            Ok(client)
        })
        .await
        .map_err(|_| ProviderError)?
    }

    pub fn cleanup(
        &self,
        owner: &crate::service::singleton_ownership::SingletonOwnership,
        cleanup: &crate::persistence::MicrovmCleanup,
    ) -> Result<bool, ProviderError> {
        owner.verify().map_err(|_| ProviderError)?;
        if self.client.config().region().map(|region| region.as_ref())
            != Some(cleanup.region.as_str())
            || self
                .account_id
                .as_deref()
                .is_some_and(|account| account != cleanup.account_id)
        {
            return Err(ProviderError);
        }
        let pending = crate::persistence::pending_microvm_cleanup_identity(
            owner.database(),
            &cleanup.intent_id,
            &cleanup.provider_id,
        )
        .map_err(|_| ProviderError)?;
        if !pending {
            return Ok(false);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| ProviderError)?;
        let terminated = runtime.block_on(self.is_terminated(&cleanup.provider_id))?;
        owner.verify().map_err(|_| ProviderError)?;
        if !terminated {
            let client = runtime.block_on(self.authorized_client())?;
            owner.verify().map_err(|_| ProviderError)?;
            runtime.block_on(Self::terminate_with_client(&client, &cleanup.provider_id))?;
            return Ok(false);
        }
        crate::persistence::record_microvm_termination(
            owner.database(),
            &cleanup.intent_id,
            &cleanup.provider_id,
        )
        .map_err(|_| ProviderError)
    }

    pub async fn verify_image(
        &self,
        image_arn: &str,
        image_version: &str,
    ) -> Result<(), ProviderError> {
        use aws_sdk_lambdamicrovms::types::{
            Architecture, MicrovmImageVersionState, MicrovmImageVersionStatus,
        };
        let image = self
            .authorized_client()
            .await?
            .get_microvm_image_version()
            .image_identifier(image_arn)
            .image_version(image_version)
            .send()
            .await
            .map_err(|_| ProviderError)?;
        if image.image_arn() != image_arn
            || image.image_version() != image_version
            || image.state() != &MicrovmImageVersionState::Successful
            || image.status() != &MicrovmImageVersionStatus::Active
            || image.cpu_configurations().len() != 1
            || image.cpu_configurations()[0].architecture() != &Architecture::Arm64
        {
            return Err(ProviderError);
        }
        Ok(())
    }

    /// Verifies provider metadata, not the instance's linkage to a launch intent.
    pub async fn verify_identity(
        &self,
        provider_id: &str,
        account_id: &str,
        region: &str,
        image_arn: &str,
        image_version: &str,
    ) -> Result<(), ProviderError> {
        self.verify_authority(account_id, region)?;
        if provider_id.trim().is_empty()
            || provider_id.len() > 256
            || provider_id.chars().any(char::is_control)
        {
            return Err(ProviderError);
        }
        let result = self
            .authorized_client()
            .await?
            .get_microvm()
            .microvm_identifier(provider_id)
            .send()
            .await
            .map_err(|_| ProviderError)?;
        if result.microvm_id() != provider_id
            || result.image_arn() != image_arn
            || result.image_version() != image_version
        {
            return Err(ProviderError);
        }
        Ok(())
    }

    fn verify_authority(&self, account_id: &str, region: &str) -> Result<(), ProviderError> {
        if self.account_id.as_deref() != Some(account_id)
            || self.authority.is_none()
            || self.client.config().region().map(|region| region.as_ref()) != Some(region)
        {
            return Err(ProviderError);
        }
        Ok(())
    }

    pub async fn verify_termination(
        &self,
        provider_id: &str,
        account_id: &str,
        region: &str,
    ) -> Result<(), ProviderError> {
        self.verify_authority(account_id, region)?;
        if !self.is_terminated(provider_id).await? {
            return Err(ProviderError);
        }
        Ok(())
    }

    pub async fn is_terminated(&self, provider_id: &str) -> Result<bool, ProviderError> {
        if provider_id.is_empty()
            || provider_id.len() > 256
            || provider_id.chars().any(char::is_control)
        {
            return Err(ProviderError);
        }
        let result = self
            .authorized_client()
            .await?
            .get_microvm()
            .microvm_identifier(provider_id)
            .send()
            .await
            .map_err(|_| ProviderError)?;
        if result.microvm_id() != provider_id {
            return Err(ProviderError);
        }
        Ok(result.state() == &aws_sdk_lambdamicrovms::types::MicrovmState::Terminated)
    }

    pub async fn terminate(&self, provider_id: &str) -> Result<(), ProviderError> {
        let client = self.authorized_client().await?;
        Self::terminate_with_client(&client, provider_id).await
    }

    async fn terminate_with_client(
        client: &Client,
        provider_id: &str,
    ) -> Result<(), ProviderError> {
        if provider_id.is_empty()
            || provider_id.len() > 256
            || provider_id.chars().any(char::is_control)
        {
            return Err(ProviderError);
        }
        client
            .terminate_microvm()
            .microvm_identifier(provider_id)
            .send()
            .await
            .map_err(|_| ProviderError)?;
        Ok(())
    }

    pub fn launch_reserved(
        &self,
        owner: &crate::service::singleton_ownership::SingletonOwnership,
        intent: &crate::persistence::MicrovmLaunchIntent,
        payload: &str,
    ) -> Result<Option<String>, ProviderError> {
        owner.verify().map_err(|_| ProviderError)?;
        validate_launch_payload(payload, intent)?;
        if self.client.config().region().map(|region| region.as_ref())
            != Some(intent.region.as_str())
            || self
                .account_id
                .as_deref()
                .is_some_and(|account| account != intent.account_id)
            || launch_request_digest(
                &intent.image_arn,
                &intent.image_version,
                &intent.client_token,
                payload,
                intent.maximum_duration_seconds,
            ) != intent.request_digest
        {
            return Err(ProviderError);
        }
        let database = owner.database();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| ProviderError)?;
        let client = runtime.block_on(self.authorized_client())?;
        owner.verify().map_err(|_| ProviderError)?;
        if !crate::persistence::dispatch_microvm_launch_intent(database, intent)
            .map_err(|_| ProviderError)?
        {
            return Ok(None);
        }
        owner.verify().map_err(|_| ProviderError)?;
        let started = std::time::Instant::now();
        let response = runtime.block_on(Self::launch(
            &client,
            &intent.image_arn,
            &intent.image_version,
            &intent.client_token,
            payload,
            intent.maximum_duration_seconds,
        ));
        crate::service::metrics::microvm_launch(started.elapsed(), response.is_ok());
        owner.verify().map_err(|_| ProviderError)?;
        match response {
            Ok(id) => {
                if !crate::persistence::bind_microvm_launch(database, &intent.intent_id, &id)
                    .map_err(|_| ProviderError)?
                {
                    return Err(ProviderError);
                }
                Ok(Some(id))
            }
            Err(error) => {
                crate::persistence::cancel_microvm_launch(database, &intent.intent_id)
                    .map_err(|_| ProviderError)?;
                Err(error)
            }
        }
    }

    async fn launch(
        client: &Client,
        image: &str,
        version: &str,
        token: &str,
        payload: &str,
        lifetime_seconds: i32,
    ) -> Result<String, ProviderError> {
        if image.is_empty()
            || image.len() > 2048
            || version.is_empty()
            || !version.bytes().all(|byte| byte.is_ascii_digit())
            || token.is_empty()
            || token.len() > 128
            || payload.len() > 16384
            || !(1..=28800).contains(&lifetime_seconds)
        {
            return Err(ProviderError);
        }
        let idle = aws_sdk_lambdamicrovms::types::IdlePolicy::builder()
            .max_idle_duration_seconds(lifetime_seconds.max(60) + 1)
            .suspended_duration_seconds(60)
            .auto_resume_enabled(false)
            .build()
            .map_err(|_| ProviderError)?;
        let result = client
            .run_microvm()
            .image_identifier(image)
            .image_version(version)
            .client_token(token)
            .run_hook_payload(payload)
            .maximum_duration_in_seconds(lifetime_seconds)
            .idle_policy(idle)
            .send()
            .await
            .map_err(|_| ProviderError)?;
        let id = result.microvm_id();
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err(ProviderError);
        }
        Ok(id.to_owned())
    }
}
