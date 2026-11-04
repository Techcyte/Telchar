use super::{Provider, ProviderError};
use std::io::Read;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    status: String,
    reset_complete: bool,
    intent_id: String,
    provider_id: String,
    assignment_epoch: i64,
}

fn reset_matches(bytes: &[u8], intent: &str, provider: &str, epoch: i64) -> bool {
    serde_json::from_slice::<Report>(bytes).is_ok_and(|report| {
        report.status == "succeeded"
            && report.reset_complete
            && report.intent_id == intent
            && report.provider_id == provider
            && report.assignment_epoch == epoch
            && epoch > 0
    })
}

impl Provider {
    pub fn observe_worker_reset(
        &self,
        owner: &crate::service::singleton_ownership::SingletonOwnership,
        intent_id: &str,
        epoch: i64,
        attempt_id: i64,
        provider_id: &str,
    ) -> Result<bool, ProviderError> {
        owner.verify().map_err(|_| ProviderError)?;
        let database = owner.database();
        let Some(identity) =
            crate::persistence::microvm_termination_identity(database, intent_id, provider_id)
                .map_err(|_| ProviderError)?
        else {
            return Ok(false);
        };
        let mut connection = crate::persistence::connect(database).map_err(|_| ProviderError)?;
        let image = connection.query_one("SELECT image_arn,image_version FROM microvm_launches WHERE intent_id=$1 AND provider_id=$2", &[&intent_id, &provider_id]).map_err(|_| ProviderError)?;
        let image_arn: String = image.get(0);
        let image_version: String = image.get(1);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| ProviderError)?;
        let started = std::time::Instant::now();
        let (endpoint, token) = runtime
            .block_on(async {
                tokio::time::timeout(self.timeout, async {
                    self.verify_authority(&identity.account_id, &identity.region)?;
                    let client = self.authorized_client().await?;
                    let vm = client
                        .get_microvm()
                        .microvm_identifier(provider_id)
                        .send()
                        .await
                        .map_err(|_| ProviderError)?;
                    if vm.microvm_id() != provider_id
                        || vm.image_arn() != image_arn
                        || vm.image_version() != image_version
                    {
                        return Err(ProviderError);
                    }
                    let token = client
                        .create_microvm_auth_token()
                        .microvm_identifier(provider_id)
                        .expiration_in_minutes(1)
                        .allowed_ports(aws_sdk_lambdamicrovms::types::PortSpecification::Port(8080))
                        .send()
                        .await
                        .map_err(|_| ProviderError)?;
                    Ok((
                        vm.endpoint().to_owned(),
                        token
                            .auth_token()
                            .get("X-aws-proxy-auth")
                            .ok_or(ProviderError)?
                            .clone(),
                    ))
                })
                .await
            })
            .map_err(|_| ProviderError)??;
        owner.verify().map_err(|_| ProviderError)?;
        let remaining = self
            .timeout
            .checked_sub(started.elapsed())
            .filter(|duration| !duration.is_zero())
            .ok_or(ProviderError)?;
        let mut url = url::Url::parse(&endpoint).map_err(|_| ProviderError)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(ProviderError);
        }
        url.set_path("/aws/lambda-microvms/runtime/v1/validate");
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(remaining)
            .build()
            .map_err(|_| ProviderError)?;
        let response = client
            .post(url)
            .header("X-aws-proxy-auth", token)
            .header("X-aws-proxy-port", "8080")
            .json(&serde_json::json!({}))
            .send()
            .map_err(|_| ProviderError)?;
        if response.status().as_u16() != 409 {
            return Ok(false);
        }
        let mut bytes = Vec::new();
        response
            .take(16385)
            .read_to_end(&mut bytes)
            .map_err(|_| ProviderError)?;
        if bytes.len() > 16384 || !reset_matches(&bytes, intent_id, provider_id, epoch) {
            return Ok(false);
        }
        owner.verify().map_err(|_| ProviderError)?;
        crate::persistence::reset_microvm_worker(database, intent_id, epoch, attempt_id)
            .map_err(|_| ProviderError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reset_requires_exact_identity_epoch_and_successful_teardown() {
        let report = serde_json::json!({"status":"succeeded","reset_complete":true,
            "intent_id":"intent","provider_id":"provider","assignment_epoch":2});
        let bytes = serde_json::to_vec(&report).unwrap();
        assert!(reset_matches(&bytes, "intent", "provider", 2));
        assert!(!reset_matches(&bytes, "intent", "provider", 1));
        assert!(!reset_matches(&bytes, "other", "provider", 2));
        let mut failed = report;
        failed["reset_complete"] = false.into();
        assert!(!reset_matches(
            &serde_json::to_vec(&failed).unwrap(),
            "intent",
            "provider",
            2
        ));
    }
}
