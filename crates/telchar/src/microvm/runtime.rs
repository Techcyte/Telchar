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

fn read_reset_report(
    mut response: reqwest::blocking::Response,
    intent: &str,
    provider: &str,
    epoch: i64,
) -> Result<bool, ProviderError> {
    if response.status().as_u16() != 409 {
        return Ok(false);
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(16385)
        .read_to_end(&mut bytes)
        .map_err(|_| ProviderError)?;
    Ok(bytes.len() <= 16384 && reset_matches(&bytes, intent, provider, epoch))
}

fn post_status(
    client: &reqwest::blocking::Client,
    url: url::Url,
    token: &str,
) -> Result<reqwest::blocking::Response, ProviderError> {
    client
        .post(url)
        .header("X-aws-proxy-auth", token)
        .header("X-aws-proxy-port", "8080")
        .json(&serde_json::json!({}))
        .send()
        .map_err(|_| ProviderError)
}

impl Provider {
    pub fn observe_worker_reset(
        &self,
        owner: &crate::service::singleton_ownership::SingletonOwnership,
        intent_id: &str,
        epoch: i64,
        attempt_id: i64,
        provider_id: &str,
        retention: std::time::Duration,
        maximum_jobs: u32,
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
        let response = post_status(&client, url, &token)?;
        if !read_reset_report(response, intent_id, provider_id, epoch)? {
            return Ok(false);
        }
        owner.verify().map_err(|_| ProviderError)?;
        crate::persistence::reset_microvm_worker_until(
            database,
            intent_id,
            epoch,
            attempt_id,
            retention,
            maximum_jobs,
        )
        .map_err(|_| ProviderError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn https_proxy_status_preserves_authentication_and_rejects_oversized_reports() {
        use std::io::Write;
        use std::sync::Arc;
        for oversized in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let certificate =
                include_bytes!("../../../telchar-nomad-worker/tests/fixtures/microvm-cert.der");
            let key =
                include_bytes!("../../../telchar-nomad-worker/tests/fixtures/microvm-key.der");
            let server = std::thread::spawn(move || {
                let config = rustls::ServerConfig::builder_with_provider(
                    rustls::crypto::ring::default_provider().into(),
                )
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![certificate.to_vec().into()],
                    rustls::pki_types::PrivateKeyDer::Pkcs8(key.to_vec().into()),
                )
                .unwrap();
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut stream = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(Arc::new(config)).unwrap(),
                    stream,
                );
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(request.starts_with("post /aws/lambda-microvms/runtime/v1/validate "));
                assert!(request.contains("x-aws-proxy-auth: fixture-token\r\n"));
                assert!(request.contains("x-aws-proxy-port: 8080\r\n"));
                let mut body = [0; 2];
                stream.read_exact(&mut body).unwrap();
                assert_eq!(&body, b"{}");
                let body = if oversized {
                    vec![b' '; 16385]
                } else {
                    serde_json::to_vec(&serde_json::json!({"status":"succeeded","reset_complete":true,"intent_id":"intent","provider_id":"provider","assignment_epoch":2})).unwrap()
                };
                write!(
                    stream,
                    "HTTP/1.1 409 Conflict\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
                stream.flush().unwrap();
            });
            let client = reqwest::blocking::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(3))
                .add_root_certificate(reqwest::Certificate::from_der(certificate).unwrap())
                .build()
                .unwrap();
            let url = url::Url::parse(&format!(
                "https://localhost:{}/aws/lambda-microvms/runtime/v1/validate",
                address.port()
            ))
            .unwrap();
            let response = post_status(&client, url, "fixture-token").unwrap();
            assert_eq!(
                read_reset_report(response, "intent", "provider", 2).unwrap(),
                !oversized
            );
            server.join().unwrap();
        }
    }

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
