use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::nomad_transfer_protocol::{Authentication, AuthenticationProof};

const MAXIMUM_CAPABILITY_BYTES: usize = 4096;
const MAXIMUM_IDENTITY_BYTES: usize = 256;

#[derive(Clone, Debug)]
pub struct HmacVerificationPolicy {
    pub key_id: String,
    pub secret: Vec<u8>,
    pub backend: String,
    pub namespace: String,
    pub job_id: String,
    pub shared_build_digest: String,
    pub task: String,
    pub clock_skew: Duration,
    pub nonce_retention: Duration,
    pub maximum_retained_nonces: usize,
}

#[derive(Debug)]
pub struct HmacCallbackVerifier {
    policy: HmacVerificationPolicy,
    nonces: BTreeMap<String, u64>,
}

impl HmacCallbackVerifier {
    pub fn new(policy: HmacVerificationPolicy) -> io::Result<Self> {
        if policy.secret.is_empty()
            || policy.maximum_retained_nonces == 0
            || policy.nonce_retention.is_zero()
        {
            return Err(invalid("Nomad HMAC verification policy is invalid"));
        }
        for value in [
            &policy.key_id,
            &policy.backend,
            &policy.namespace,
            &policy.job_id,
            &policy.shared_build_digest,
            &policy.task,
        ] {
            validate_identity(value)?;
        }
        Ok(Self {
            policy,
            nonces: BTreeMap::new(),
        })
    }

    pub fn verify(
        &mut self,
        authentication: &Authentication,
        method: &str,
        path: &str,
        now: SystemTime,
    ) -> io::Result<VerifiedHmacRequest> {
        let AuthenticationProof::Hmac {
            capability,
            expiry,
            nonce,
            body_digest,
            signature,
        } = &authentication.proof
        else {
            return Err(invalid("Nomad callback authentication mode is invalid"));
        };
        validate_authentication_identity(authentication, &self.policy)?;
        if method != "POST" || path.is_empty() || !path.starts_with('/') || path.contains('#') {
            return Err(invalid("Nomad callback request target is invalid"));
        }
        validate_identity(nonce)?;
        let now = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("Nomad callback clock is invalid"))?
            .as_secs();
        self.remove_expired_nonces(now);

        let (encoded_claims, encoded_capability_signature) = capability
            .split_once('.')
            .ok_or_else(|| invalid("Nomad callback capability is invalid"))?;
        if capability.len() > MAXIMUM_CAPABILITY_BYTES
            || encoded_claims.is_empty()
            || encoded_capability_signature.is_empty()
            || encoded_capability_signature.contains('.')
        {
            return Err(invalid("Nomad callback capability is invalid"));
        }
        verify_mac(
            &self.policy.secret,
            encoded_claims.as_bytes(),
            encoded_capability_signature,
        )?;
        let claims_bytes = URL_SAFE_NO_PAD
            .decode(encoded_claims)
            .map_err(|_| invalid("Nomad callback capability is invalid"))?;
        if claims_bytes.len() > MAXIMUM_CAPABILITY_BYTES {
            return Err(invalid("Nomad callback capability is invalid"));
        }
        let claims: CapabilityClaims = serde_json::from_slice(&claims_bytes)
            .map_err(|_| invalid("Nomad callback capability is invalid"))?;
        validate_claims(&claims, &self.policy, *expiry, now)?;

        let expected_body_digest =
            authentication_body_digest(authentication, capability, *expiry, nonce)?;
        if expected_body_digest != *body_digest {
            return Err(invalid("Nomad callback body digest is invalid"));
        }
        let request_key = URL_SAFE_NO_PAD
            .decode(&claims.request_key)
            .map_err(|_| invalid("Nomad callback capability is invalid"))?;
        let signed_request =
            format!("{capability}\n{method}\n{path}\n{body_digest}\n{expiry}\n{nonce}");
        verify_mac(&request_key, signed_request.as_bytes(), signature)?;

        if self.nonces.contains_key(nonce) {
            return Err(invalid("Nomad callback request was replayed"));
        }
        if self.nonces.len() >= self.policy.maximum_retained_nonces {
            return Err(invalid("Nomad callback replay capacity is exhausted"));
        }
        let retained_until = now
            .checked_add(self.policy.nonce_retention.as_secs())
            .ok_or_else(|| invalid("Nomad callback nonce retention is invalid"))?;
        self.nonces.insert(nonce.clone(), retained_until);
        Ok(VerifiedHmacRequest {
            nonce: nonce.clone(),
            expires_at: UNIX_EPOCH + Duration::from_secs(retained_until),
        })
    }

    fn remove_expired_nonces(&mut self, now: u64) {
        self.nonces
            .retain(|_, retained_until| *retained_until > now);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedHmacRequest {
    nonce: String,
    expires_at: SystemTime,
}

impl VerifiedHmacRequest {
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }
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

fn validate_authentication_identity(
    authentication: &Authentication,
    policy: &HmacVerificationPolicy,
) -> io::Result<()> {
    for value in [
        &authentication.backend,
        &authentication.namespace,
        &authentication.job_id,
        &authentication.allocation_id,
        &authentication.task,
        &authentication.shared_build_digest,
    ] {
        validate_identity(value)?;
    }
    if authentication.backend != policy.backend
        || authentication.namespace != policy.namespace
        || authentication.job_id != policy.job_id
        || authentication.task != policy.task
        || authentication.shared_build_digest != policy.shared_build_digest
    {
        return Err(invalid(
            "Nomad callback identity does not match expected execution",
        ));
    }
    Ok(())
}

fn validate_claims(
    claims: &CapabilityClaims,
    policy: &HmacVerificationPolicy,
    request_expiry: u64,
    now: u64,
) -> io::Result<()> {
    for value in [
        &claims.key_id,
        &claims.backend,
        &claims.namespace,
        &claims.job_id,
        &claims.shared_build_digest,
        &claims.nonce,
        &claims.request_key,
    ] {
        validate_identity(value)?;
    }
    let skew = policy.clock_skew.as_secs();
    let latest_issued_at = now
        .checked_add(skew)
        .ok_or_else(|| invalid("Nomad callback clock is invalid"))?;
    let latest_accepted_expiry = claims
        .expires_at
        .checked_add(skew)
        .ok_or_else(|| invalid("Nomad callback capability is invalid"))?;
    if claims.version != 1
        || claims.key_id != policy.key_id
        || claims.backend != policy.backend
        || claims.namespace != policy.namespace
        || claims.job_id != policy.job_id
        || claims.shared_build_digest != policy.shared_build_digest
        || claims.issued_at > claims.expires_at
        || claims.issued_at > latest_issued_at
        || now > latest_accepted_expiry
        || request_expiry != claims.expires_at
    {
        return Err(invalid("Nomad callback capability is invalid"));
    }
    Ok(())
}

fn authentication_body_digest(
    authentication: &Authentication,
    capability: &str,
    expiry: u64,
    nonce: &str,
) -> io::Result<String> {
    let encoded = serde_json::to_vec(&serde_json::json!({
        "backend": authentication.backend,
        "namespace": authentication.namespace,
        "job_id": authentication.job_id,
        "allocation_id": authentication.allocation_id,
        "task": authentication.task,
        "shared_build_digest": authentication.shared_build_digest,
        "capability": capability,
        "expiry": expiry,
        "nonce": nonce,
    }))
    .map_err(|_| invalid("Nomad callback authentication body is invalid"))?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(encoded)))
}

fn verify_mac(key: &[u8], message: &[u8], encoded_signature: &str) -> io::Result<()> {
    let signature = URL_SAFE_NO_PAD
        .decode(encoded_signature)
        .map_err(|_| invalid("Nomad callback signature is invalid"))?;
    let mut verifier = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|_| invalid("Nomad callback verification key is invalid"))?;
    verifier.update(message);
    verifier
        .verify_slice(&signature)
        .map_err(|_| invalid("Nomad callback signature is invalid"))
}

fn validate_identity(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_IDENTITY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(invalid("Nomad callback identity is invalid"));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
