//! Delivers content-addressed archives with signed conditional S3 requests.

use std::io::{self, Read};
use std::time::{Duration, SystemTime};

use aws_sigv4::http_request::{SignableBody, SignableRequest, SigningSettings, sign};
use sha2::{Digest, Sha256};

pub struct S3Archive {
    endpoint: url::Url,
    region: String,
    credentials: aws_credential_types::Credentials,
    client: reqwest::blocking::Client,
}

impl S3Archive {
    pub fn new(
        endpoint: &str,
        region: &str,
        credentials: aws_credential_types::Credentials,
        timeout: Duration,
    ) -> io::Result<Self> {
        let endpoint = url::Url::parse(endpoint).map_err(|_| invalid())?;
        if !matches!(endpoint.scheme(), "https" | "http")
            || (endpoint.scheme() == "http"
                && !matches!(
                    endpoint.host_str(),
                    Some("127.0.0.1" | "[::1]" | "localhost")
                ))
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !endpoint.path().ends_with('/')
            || region.is_empty()
            || timeout.is_zero()
            || timeout > Duration::from_secs(300)
        {
            return Err(invalid());
        }
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| invalid())?;
        Ok(Self {
            endpoint,
            region: region.to_owned(),
            credentials,
            client,
        })
    }

    pub fn deliver(&self, key: &str, payload: &[u8]) -> io::Result<()> {
        if payload.len() > 8 * 1024 * 1024
            || key.split('/').any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            })
        {
            return Err(invalid());
        }
        let url = self.endpoint.join(key).map_err(|_| invalid())?;
        let checksum: String = Sha256::digest(payload)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let response = self.request("PUT", &url, payload, &checksum)?;
        if response.status().is_success() {
            return Ok(());
        }
        if response.status() != reqwest::StatusCode::PRECONDITION_FAILED {
            return Err(io::Error::other("archive storage rejected delivery"));
        }
        let response = self.request("GET", &url, &[], &checksum)?;
        if !response.status().is_success() {
            return Err(io::Error::other("archive storage verification failed"));
        }
        let mut existing = Vec::new();
        response
            .take(payload.len() as u64 + 1)
            .read_to_end(&mut existing)
            .map_err(|_| io::Error::other("archive storage verification failed"))?;
        if existing != payload {
            return Err(io::Error::other("archive content conflict"));
        }
        Ok(())
    }

    fn request(
        &self,
        method: &str,
        url: &url::Url,
        payload: &[u8],
        checksum: &str,
    ) -> io::Result<reqwest::blocking::Response> {
        let identity = self.credentials.clone().into();
        let params = aws_sigv4::sign::v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name("s3")
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .map_err(|_| invalid())?
            .into();
        let mut headers = vec![("x-amz-meta-sha256", checksum)];
        if method == "PUT" {
            headers.push(("if-none-match", "*"));
        }
        let signable = SignableRequest::new(
            method,
            url.as_str(),
            headers.iter().copied(),
            SignableBody::Bytes(payload),
        )
        .map_err(|_| invalid())?;
        let (instructions, _) = sign(signable, &params).map_err(|_| invalid())?.into_parts();
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| invalid())?;
        let mut request = self
            .client
            .request(method, url.clone())
            .body(payload.to_vec());
        for (name, value) in headers {
            request = request.header(name, value);
        }
        for (name, value) in instructions.headers() {
            request = request.header(name, value);
        }
        request
            .send()
            .map_err(|_| io::Error::other("archive storage request failed"))
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid archive storage configuration or request",
    )
}
