use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use telchar::nomad_transfer_protocol::{
    encode_metadata, write_frame, Authentication, AuthenticationProof, Frame, FrameKind,
    ProtocolLimits,
};
use url::Url;

const MAXIMUM_ENVIRONMENT_VALUE_BYTES: usize = 4096;
const MAXIMUM_AUTHENTICATION_METADATA_BYTES: usize = 16 * 1024;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    endpoint: Url,
    store_uri: String,
    authentication: Authentication,
}

impl WorkerConfig {
    pub fn from_environment() -> io::Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> io::Result<Self> {
        let endpoint = required(&mut lookup, "TELCHAR_TRANSFER_ENDPOINT")?;
        let endpoint =
            Url::parse(&endpoint).map_err(|_| invalid("worker callback endpoint is invalid"))?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(invalid("worker callback endpoint is invalid"));
        }
        let store_uri = required(&mut lookup, "TELCHAR_NIX_STORE_URI")?;
        let mode = required(&mut lookup, "TELCHAR_TRANSFER_AUTHENTICATION")?;
        let allocation_id = required(&mut lookup, "NOMAD_ALLOC_ID")?;
        let nomad_namespace = required(&mut lookup, "NOMAD_NAMESPACE")?;
        let nomad_job_id = required(&mut lookup, "NOMAD_JOB_ID")?;
        let nomad_task = required(&mut lookup, "NOMAD_TASK_NAME")?;

        let (backend, namespace, job_id, shared_build_digest, task, proof) = match mode.as_str() {
            "workload-identity" => {
                let namespace = required(&mut lookup, "TELCHAR_NAMESPACE")?;
                let job_id = required(&mut lookup, "TELCHAR_JOB_ID")?;
                let task = required(&mut lookup, "TELCHAR_TASK")?;
                if namespace != nomad_namespace || job_id != nomad_job_id || task != nomad_task {
                    return Err(invalid(
                        "worker Nomad identity does not match submitted job",
                    ));
                }
                (
                    required(&mut lookup, "TELCHAR_BACKEND")?,
                    namespace,
                    job_id,
                    required(&mut lookup, "TELCHAR_SHARED_BUILD_DIGEST")?,
                    task,
                    AuthenticationProof::WorkloadIdentity {
                        token: required(&mut lookup, "NOMAD_TOKEN")?,
                    },
                )
            }
            "hmac" => {
                let capability = required(&mut lookup, "TELCHAR_TRANSFER_CAPABILITY")?;
                let claims = decode_capability(&capability)?;
                if claims.namespace != nomad_namespace
                    || claims.job_id != nomad_job_id
                    || nomad_task != "build"
                {
                    return Err(invalid(
                        "worker Nomad identity does not match signed capability",
                    ));
                }
                let expiry = claims.expires_at;
                let nonce = claims.nonce.clone();
                let body_digest = authentication_body_digest(
                    &claims.backend,
                    &claims.namespace,
                    &claims.job_id,
                    &allocation_id,
                    &nomad_task,
                    &claims.shared_build_digest,
                    &capability,
                    expiry,
                    &nonce,
                )?;
                let signature = request_signature(
                    &claims.request_key,
                    &capability,
                    endpoint.path(),
                    &body_digest,
                    expiry,
                    &nonce,
                )?;
                (
                    claims.backend,
                    claims.namespace,
                    claims.job_id,
                    claims.shared_build_digest,
                    nomad_task,
                    AuthenticationProof::Hmac {
                        capability,
                        expiry,
                        nonce,
                        body_digest,
                        signature,
                    },
                )
            }
            _ => return Err(invalid("worker transfer authentication mode is invalid")),
        };

        Ok(Self {
            endpoint,
            store_uri,
            authentication: Authentication {
                backend,
                namespace,
                job_id,
                allocation_id,
                task,
                shared_build_digest,
                proof,
            },
        })
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    pub fn store_uri(&self) -> &str {
        &self.store_uri
    }

    pub fn authentication(&self) -> &Authentication {
        &self.authentication
    }
}

pub fn authenticate(config: &WorkerConfig) -> io::Result<()> {
    let metadata = encode_metadata(
        config.authentication(),
        MAXIMUM_AUTHENTICATION_METADATA_BYTES,
    )?;
    let mut body = Vec::with_capacity(metadata.len() + 16);
    write_frame(
        &mut body,
        &Frame::new(FrameKind::Authenticate, metadata, Vec::new()),
        ProtocolLimits::new(MAXIMUM_AUTHENTICATION_METADATA_BYTES, 0),
    )?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(CALLBACK_TIMEOUT)
        .timeout(CALLBACK_TIMEOUT)
        .build()
        .map_err(|_| io::Error::other("worker callback client could not be created"))?;
    let response = client
        .post(config.endpoint().clone())
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/vnd.telchar.nomad-transfer",
        )
        .body(body)
        .send()
        .map_err(|_| io::Error::other("worker callback authentication failed"))?;
    if !response.status().is_success() {
        return Err(io::Error::other(
            "worker callback authentication was rejected",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityClaims {
    version: u16,
    key_id: String,
    backend: String,
    namespace: String,
    job_id: String,
    shared_build_digest: String,
    issued_at: u64,
    expires_at: u64,
    nonce: String,
    request_key: String,
}

fn decode_capability(capability: &str) -> io::Result<CapabilityClaims> {
    let (encoded, signature) = capability
        .split_once('.')
        .ok_or_else(|| invalid("worker transfer capability is invalid"))?;
    if encoded.is_empty() || signature.is_empty() || signature.contains('.') {
        return Err(invalid("worker transfer capability is invalid"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| invalid("worker transfer capability is invalid"))?;
    if bytes.len() > MAXIMUM_ENVIRONMENT_VALUE_BYTES {
        return Err(invalid("worker transfer capability is invalid"));
    }
    let claims: CapabilityClaims = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("worker transfer capability is invalid"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid("worker clock is invalid"))?
        .as_secs();
    if claims.version != 1
        || claims.key_id.is_empty()
        || claims.issued_at > claims.expires_at
        || now > claims.expires_at
    {
        return Err(invalid("worker transfer capability is invalid"));
    }
    Ok(claims)
}

#[allow(clippy::too_many_arguments)]
fn authentication_body_digest(
    backend: &str,
    namespace: &str,
    job_id: &str,
    allocation_id: &str,
    task: &str,
    shared_build_digest: &str,
    capability: &str,
    expiry: u64,
    nonce: &str,
) -> io::Result<String> {
    let encoded = serde_json::to_vec(&serde_json::json!({
        "backend": backend,
        "namespace": namespace,
        "job_id": job_id,
        "allocation_id": allocation_id,
        "task": task,
        "shared_build_digest": shared_build_digest,
        "capability": capability,
        "expiry": expiry,
        "nonce": nonce,
    }))
    .map_err(|_| invalid("worker authentication body is invalid"))?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(encoded)))
}

fn request_signature(
    encoded_key: &str,
    capability: &str,
    path: &str,
    body_digest: &str,
    expiry: u64,
    nonce: &str,
) -> io::Result<String> {
    let key = URL_SAFE_NO_PAD
        .decode(encoded_key)
        .map_err(|_| invalid("worker transfer capability is invalid"))?;
    let mut signer = Hmac::<Sha256>::new_from_slice(&key)
        .map_err(|_| invalid("worker transfer capability is invalid"))?;
    signer.update(capability.as_bytes());
    signer.update(b"\nPOST\n");
    signer.update(path.as_bytes());
    signer.update(b"\n");
    signer.update(body_digest.as_bytes());
    signer.update(b"\n");
    signer.update(expiry.to_string().as_bytes());
    signer.update(b"\n");
    signer.update(nonce.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(signer.finalize().into_bytes()))
}

fn required(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> io::Result<String> {
    let value = lookup(name).ok_or_else(|| invalid("worker environment is incomplete"))?;
    if value.is_empty()
        || value.len() > MAXIMUM_ENVIRONMENT_VALUE_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(invalid("worker environment value is invalid"));
    }
    Ok(value)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
