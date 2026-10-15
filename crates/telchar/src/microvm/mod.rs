//! AWS control-plane operations for exact MicroVM executions.

pub mod rendezvous;

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
    crate::persistence::admit_microvm_session(
        owner.database_url(),
        intent_id,
        provider_id,
        &verifier,
    )
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

pub struct Provider {
    client: Client,
}

impl Provider {
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
        })
    }

    pub fn cleanup(
        &self,
        owner: &crate::service::singleton_ownership::SingletonOwnership,
        cleanup: &crate::persistence::MicrovmCleanup,
    ) -> Result<bool, ProviderError> {
        owner.verify().map_err(|_| ProviderError)?;
        let pending = crate::persistence::pending_microvm_cleanup_identity(
            owner.database_url(),
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
            runtime.block_on(self.terminate(&cleanup.provider_id))?;
            return Ok(false);
        }
        crate::persistence::record_microvm_termination(
            owner.database_url(),
            &cleanup.intent_id,
            &cleanup.provider_id,
        )
        .map_err(|_| ProviderError)
    }

    pub async fn is_terminated(&self, provider_id: &str) -> Result<bool, ProviderError> {
        if provider_id.is_empty()
            || provider_id.len() > 256
            || provider_id.chars().any(char::is_control)
        {
            return Err(ProviderError);
        }
        let result = self
            .client
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
        if provider_id.is_empty()
            || provider_id.len() > 256
            || provider_id.chars().any(char::is_control)
        {
            return Err(ProviderError);
        }
        self.client
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
        let database = owner.database_url();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| ProviderError)?;
        if !crate::persistence::dispatch_microvm_launch_intent(database, intent)
            .map_err(|_| ProviderError)?
        {
            return Ok(None);
        }
        owner.verify().map_err(|_| ProviderError)?;
        let response = runtime.block_on(self.launch(
            &intent.image_arn,
            &intent.image_version,
            &intent.client_token,
            payload,
            intent.maximum_duration_seconds,
        ));
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
        &self,
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
        let result = self
            .client
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
