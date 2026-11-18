//! Pure evaluation of bounded client-upload metadata, not NAR content validation.

use std::collections::{BTreeMap, BTreeSet};

use aws_lc_rs::signature::{ED25519, UnparsedPublicKey};
use base64::Engine;
use nix_worker_protocol::AddMultipleToStorePathInfo;
use serde::Serialize;
use time::OffsetDateTime;

use super::{IdentityMatch, MetadataValue, RegisteredKey, UploadVerification};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureStatus {
    Valid,
    UnknownKey,
    Invalid,
    Expired,
    Revoked,
    Malformed,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureEvaluation {
    pub key_name: Option<String>,
    pub status: SignatureStatus,
    pub source: Option<&'static str>,
    pub metadata: Option<BTreeMap<String, MetadataValue>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationReason {
    VerificationDisabled,
    ContentAddressedExemption,
    SignaturesVerified,
    InsufficientSignatures,
    IdentityMismatch,
    InvalidFingerprint,
    MetadataLimit,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evaluation {
    pub signatures: Vec<SignatureEvaluation>,
    /// Counts distinct public-key bytes, not signature strings or key names.
    pub valid_signatures: usize,
    pub identity_mismatch: bool,
    /// Only a policy eligibility claim. The caller must still validate the CA with Nix.
    pub content_addressed_exemption_eligible: bool,
    pub allowed: bool,
    pub reason: EvaluationReason,
}

/// Extracts only a bounded registry label, never raw signature data or an error message.
/// The caller can use these labels for a registry lookup before evaluating one snapshot.
pub fn key_name(signature: &[u8]) -> Option<&str> {
    let colon = signature.iter().take(257).position(|byte| *byte == b':')?;
    if colon == 0 || colon > 256 {
        return None;
    }
    let name = std::str::from_utf8(&signature[..colon]).ok()?;
    (!name.chars().any(char::is_control)).then_some(name)
}

/// Reconstructs Nix's signed fingerprint from the protocol's hex SHA-256 hash.
/// References have StorePathSet semantics: sorted full paths with duplicates removed.
/// Allocation is bounded by the production protocol parser's path/reference limits.
pub fn fingerprint(info: &AddMultipleToStorePathInfo) -> Option<Vec<u8>> {
    if info.nar_hash().len() != 64 {
        return None;
    }
    let mut hash = [0_u8; 32];
    for (output, pair) in hash.iter_mut().zip(info.nar_hash().chunks_exact(2)) {
        let high = (pair[0] as char).to_digit(16)? as u8;
        let low = (pair[1] as char).to_digit(16)? as u8;
        *output = (high << 4) | low;
    }
    let mut fingerprint = b"1;".to_vec();
    fingerprint.extend_from_slice(info.path());
    fingerprint.extend_from_slice(b";sha256:");
    fingerprint.extend_from_slice(crate::build::encode_nix_base32(&hash).as_bytes());
    fingerprint.push(b';');
    fingerprint.extend_from_slice(info.nar_size().to_string().as_bytes());
    fingerprint.push(b';');
    let mut references = info
        .references()
        .iter()
        .map(Vec::as_slice)
        .collect::<Vec<_>>();
    references.sort_unstable();
    references.dedup();
    for (index, reference) in references.iter().enumerate() {
        if index > 0 {
            fingerprint.push(b',');
        }
        fingerprint.extend_from_slice(reference);
    }
    Some(fingerprint)
}

/// Evaluates one immutable registry snapshot at a caller-supplied time and authenticated subject.
/// Configuration must have passed `UploadVerification::validate`. This does not establish
/// NAR integrity, CA validity, or build provenance, and never performs I/O or logs errors.
pub fn evaluate(
    info: &AddMultipleToStorePathInfo,
    keys: &BTreeMap<String, RegisteredKey>,
    config: &UploadVerification,
    audit_subject: &str,
    now: OffsetDateTime,
) -> Evaluation {
    if !config.enabled {
        return Evaluation {
            signatures: Vec::new(),
            valid_signatures: 0,
            identity_mismatch: false,
            content_addressed_exemption_eligible: false,
            allowed: true,
            reason: EvaluationReason::VerificationDisabled,
        };
    }

    let fingerprint = fingerprint(info);
    let mut valid_keys = BTreeSet::new();
    let mut identity_mismatch = false;
    let mut matching_identity = false;
    let identity_field = config.identity_field.as_deref().unwrap_or("identity");
    let mut metadata_bytes = 0usize;
    let mut metadata_limit = false;
    let signatures = info
        .signatures()
        .iter()
        .map(|signature| {
            let name = key_name(signature);
            let key = name.and_then(|name| keys.get(name));
            let status = signature_status(signature, key, fingerprint.as_deref(), now);
            if let Some(key) = key.filter(|_| status == SignatureStatus::Valid) {
                valid_keys.insert(key.bytes);
                if config.identity_match != IdentityMatch::Off {
                    let matches = matches!(
                        key.metadata.get(identity_field),
                        Some(MetadataValue::String(identity)) if identity == audit_subject
                    );
                    matching_identity |= matches;
                    identity_mismatch |= !matches;
                }
            }
            SignatureEvaluation {
                key_name: name.map(str::to_owned),
                status,
                source: key.map(|key| key.source),
                metadata: key.and_then(|key| {
                    metadata_bytes = metadata_bytes.saturating_add(
                        serde_json::to_vec(&key.metadata).map_or(usize::MAX, |bytes| bytes.len()),
                    );
                    if metadata_bytes > 64 * 1024 {
                        metadata_limit = true;
                        None
                    } else {
                        Some(key.metadata.clone())
                    }
                }),
            }
        })
        .collect();

    let content_addressed_exemption_eligible =
        config.exempt_content_addressed.unwrap_or(true) && info.content_address().is_some();
    let (allowed, reason) = if metadata_limit && !content_addressed_exemption_eligible {
        (false, EvaluationReason::MetadataLimit)
    } else if content_addressed_exemption_eligible {
        (true, EvaluationReason::ContentAddressedExemption)
    } else if fingerprint.is_none() {
        (false, EvaluationReason::InvalidFingerprint)
    } else if valid_keys.len() < config.min_signatures.unwrap_or(1) {
        (false, EvaluationReason::InsufficientSignatures)
    } else if config.identity_match == IdentityMatch::Enforce && !matching_identity {
        (false, EvaluationReason::IdentityMismatch)
    } else {
        (true, EvaluationReason::SignaturesVerified)
    };
    Evaluation {
        signatures,
        valid_signatures: valid_keys.len(),
        identity_mismatch,
        content_addressed_exemption_eligible,
        allowed,
        reason,
    }
}

fn signature_status(
    signature: &[u8],
    key: Option<&RegisteredKey>,
    fingerprint: Option<&[u8]>,
    now: OffsetDateTime,
) -> SignatureStatus {
    let Some(name) = key_name(signature) else {
        return SignatureStatus::Malformed;
    };
    let encoded = &signature[name.len() + 1..];
    // A 64-byte Ed25519 signature has exactly 88 padded base64 bytes. Decode into
    // a fixed buffer (88 base64 bytes can decode to at most 66 bytes).
    if encoded.len() != 88 {
        return SignatureStatus::Malformed;
    }
    let mut bytes = [0_u8; 66];
    if base64::engine::general_purpose::STANDARD.decode_slice(encoded, &mut bytes) != Ok(64) {
        return SignatureStatus::Malformed;
    }
    let Some(key) = key else {
        return SignatureStatus::UnknownKey;
    };
    if key.revoked {
        return SignatureStatus::Revoked;
    }
    if key.valid_from.is_some_and(|from| now < from)
        || key.valid_until.is_some_and(|until| now >= until)
    {
        return SignatureStatus::Expired;
    }
    if fingerprint.is_some_and(|fingerprint| {
        UnparsedPublicKey::new(&ED25519, key.bytes)
            .verify(fingerprint, &bytes[..64])
            .is_ok()
    }) {
        SignatureStatus::Valid
    } else {
        SignatureStatus::Invalid
    }
}
