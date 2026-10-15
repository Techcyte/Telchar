//! AWS control-plane operations for exact MicroVM executions.

use std::time::Duration;

use aws_sdk_lambdamicrovms::{Client, Config};

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

    pub async fn launch(
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
