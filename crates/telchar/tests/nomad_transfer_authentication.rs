//! Tests nomad transfer authentication contracts and failure boundaries, including authentication.

use std::time::{Duration, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::{Digest, Sha256};
use telchar::nomad_transfer_authentication::{HmacCallbackVerifier, HmacVerificationPolicy};
use telchar::nomad_transfer_protocol::{Authentication, AuthenticationProof};

const SECRET: &[u8] = b"backend-signing-secret\n";
const REQUEST_KEY: [u8; 32] = [7; 32];
const NOW: u64 = 1_000;

fn authentication(request_nonce: &str, issued_at: u64, expires_at: u64) -> Authentication {
    let claims = json!({
        "version": 1,
        "key_id": "primary",
        "backend": "nomad-primary",
        "namespace": "telchar",
        "job_id": "job-1",
        "shared_build_digest": "digest-1",
        "issued_at": issued_at,
        "expires_at": expires_at,
        "nonce": "capability-nonce",
        "request_key": URL_SAFE_NO_PAD.encode(REQUEST_KEY),
    });
    let encoded_claims =
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("claims encode"));
    let mut capability_signer = Hmac::<Sha256>::new_from_slice(SECRET).expect("signer creates");
    capability_signer.update(encoded_claims.as_bytes());
    let capability = format!(
        "{encoded_claims}.{}",
        URL_SAFE_NO_PAD.encode(capability_signer.finalize().into_bytes())
    );
    let expiry = expires_at;
    let body_digest = URL_SAFE_NO_PAD.encode(Sha256::digest(
        serde_json::to_vec(&json!({
            "backend": "nomad-primary",
            "namespace": "telchar",
            "job_id": "job-1",
            "allocation_id": "allocation-1",
            "task": "build",
            "shared_build_digest": "digest-1",
            "capability": capability,
            "expiry": expiry,
            "nonce": request_nonce,
        }))
        .expect("body encodes"),
    ));
    let mut request_signer = Hmac::<Sha256>::new_from_slice(&REQUEST_KEY).expect("signer creates");
    request_signer.update(capability.as_bytes());
    request_signer.update(b"\nPOST\n/callback\n");
    request_signer.update(body_digest.as_bytes());
    request_signer.update(b"\n");
    request_signer.update(expiry.to_string().as_bytes());
    request_signer.update(b"\n");
    request_signer.update(request_nonce.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(request_signer.finalize().into_bytes());

    Authentication {
        backend: "nomad-primary".to_owned(),
        namespace: "telchar".to_owned(),
        job_id: "job-1".to_owned(),
        allocation_id: "allocation-1".to_owned(),
        task: "build".to_owned(),
        shared_build_digest: "digest-1".to_owned(),
        proof: AuthenticationProof::Hmac {
            capability,
            expiry,
            nonce: request_nonce.to_owned(),
            body_digest,
            signature,
        },
    }
}

fn callback_verifier() -> HmacCallbackVerifier {
    HmacCallbackVerifier::new(HmacVerificationPolicy {
        key_id: "primary".to_owned(),
        secret: SECRET.to_vec(),
        backend: "nomad-primary".to_owned(),
        namespace: "telchar".to_owned(),
        job_id: "job-1".to_owned(),
        shared_build_digest: "digest-1".to_owned(),
        task: "build".to_owned(),
        clock_skew: Duration::from_secs(30),
        nonce_retention: Duration::from_secs(600),
        maximum_retained_nonces: 8,
    })
    .expect("verifier creates")
}

#[test]
fn verifies_exact_hmac_callback_and_rejects_replay() {
    let mut verifier = callback_verifier();
    let authentication = authentication("request-nonce-1", 900, 1_100);

    verifier
        .verify(
            &authentication,
            "POST",
            "/callback",
            UNIX_EPOCH + Duration::from_secs(NOW),
        )
        .expect("exact callback verifies");
    assert!(
        verifier
            .verify(
                &authentication,
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(NOW)
            )
            .is_err()
    );
}

#[test]
fn rejects_tampering_foreign_identity_and_wrong_callback_path() {
    let mut verifier = callback_verifier();
    let mut tampered = authentication("request-nonce-2", 900, 1_100);
    tampered.allocation_id = "foreign-allocation".to_owned();
    assert!(
        verifier
            .verify(
                &tampered,
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(NOW)
            )
            .is_err()
    );

    let mut verifier = callback_verifier();
    assert!(
        verifier
            .verify(
                &authentication("request-nonce-3", 900, 1_100),
                "POST",
                "/foreign",
                UNIX_EPOCH + Duration::from_secs(NOW),
            )
            .is_err()
    );

    let mut verifier = callback_verifier();
    let mut foreign = authentication("request-nonce-4", 900, 1_100);
    foreign.backend = "foreign".to_owned();
    assert!(
        verifier
            .verify(
                &foreign,
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(NOW)
            )
            .is_err()
    );
}

#[test]
fn enforces_capability_time_bounds_and_bounded_nonce_retention() {
    let mut verifier = callback_verifier();
    assert!(
        verifier
            .verify(
                &authentication("expired", 800, 900),
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(NOW),
            )
            .is_err()
    );
    assert!(
        verifier
            .verify(
                &authentication("future", 1_031, 1_100),
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(NOW),
            )
            .is_err()
    );

    for index in 0..8 {
        verifier
            .verify(
                &authentication(&format!("bounded-{index}"), 900, 1_100),
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(NOW),
            )
            .expect("bounded nonce records");
    }
    assert!(
        verifier
            .verify(
                &authentication("overflow", 900, 1_100),
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(NOW),
            )
            .is_err()
    );
}
