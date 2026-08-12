use std::cell::RefCell;
use std::io;
use std::time::{Duration, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::{Digest, Sha256};
use telchar::nomad_callback::{AllocationVerifier, CallbackAdmission};
use telchar::nomad_transfer_authentication::{HmacCallbackVerifier, HmacVerificationPolicy};
use telchar::nomad_transfer_protocol::{
    encode_metadata, write_frame, Authentication, AuthenticationProof, Direction, Frame, FrameKind,
    ProtocolLimits,
};

const SECRET: &[u8] = b"backend-signing-secret\n";
const REQUEST_KEY: [u8; 32] = [7; 32];
const NOW: u64 = 1_000;

#[derive(Default)]
struct RecordingAllocationVerifier {
    calls: RefCell<Vec<(String, String, String)>>,
    reject: bool,
}

impl AllocationVerifier for RecordingAllocationVerifier {
    fn verify_allocation(&self, allocation_id: &str, job_id: &str, task: &str) -> io::Result<()> {
        self.calls.borrow_mut().push((
            allocation_id.to_owned(),
            job_id.to_owned(),
            task.to_owned(),
        ));
        if self.reject {
            Err(io::Error::other("allocation rejected"))
        } else {
            Ok(())
        }
    }
}

fn authentication(request_nonce: &str) -> Authentication {
    let claims = json!({
        "version": 1,
        "key_id": "primary",
        "backend": "nomad-primary",
        "namespace": "telchar",
        "job_id": "job-1",
        "shared_build_digest": "digest-1",
        "issued_at": 900,
        "expires_at": 1_100,
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
    let expiry = 1_100;
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
    request_signer.update(b"\n1100\n");
    request_signer.update(request_nonce.as_bytes());

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
            signature: URL_SAFE_NO_PAD.encode(request_signer.finalize().into_bytes()),
        },
    }
}

fn frame(authentication: &Authentication) -> Vec<u8> {
    let metadata = encode_metadata(authentication, 4096).expect("authentication encodes");
    let mut body = Vec::new();
    write_frame(
        &mut body,
        &Frame::new(FrameKind::Authenticate, metadata, vec![]),
        ProtocolLimits::new(4096, 0),
    )
    .expect("frame writes");
    body
}

fn hmac_verifier() -> HmacCallbackVerifier {
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
    .expect("HMAC verifier creates")
}

#[test]
fn admits_one_authenticated_exact_allocation_session() {
    let allocation = RecordingAllocationVerifier::default();
    let mut admission = CallbackAdmission::new(hmac_verifier(), allocation, 4096);

    let mut admitted = admission
        .admit(
            &frame(&authentication("request-1")),
            "POST",
            "/callback",
            UNIX_EPOCH + Duration::from_secs(NOW),
        )
        .expect("callback admits");
    assert_eq!(admitted.authentication().allocation_id, "allocation-1");
    admitted
        .protocol_mut()
        .accept(Direction::GatewayToWorker, FrameKind::InputManifest)
        .expect("admitted session awaits manifest");
    assert_eq!(
        admission.allocation_verifier().calls.borrow().as_slice(),
        &[(
            "allocation-1".to_owned(),
            "job-1".to_owned(),
            "build".to_owned()
        )]
    );
}

#[test]
fn rejects_invalid_frame_before_allocation_lookup() {
    let allocation = RecordingAllocationVerifier::default();
    let mut admission = CallbackAdmission::new(hmac_verifier(), allocation, 4096);
    let mut body = frame(&authentication("request-2"));
    body.push(0);

    assert!(admission
        .admit(
            &body,
            "POST",
            "/callback",
            UNIX_EPOCH + Duration::from_secs(NOW),
        )
        .is_err());
    assert!(admission.allocation_verifier().calls.borrow().is_empty());
}

#[test]
fn rejects_unverified_allocation_and_consumes_authenticated_nonce() {
    let allocation = RecordingAllocationVerifier {
        reject: true,
        ..RecordingAllocationVerifier::default()
    };
    let mut admission = CallbackAdmission::new(hmac_verifier(), allocation, 4096);
    let body = frame(&authentication("request-3"));

    assert!(admission
        .admit(
            &body,
            "POST",
            "/callback",
            UNIX_EPOCH + Duration::from_secs(NOW),
        )
        .is_err());
    assert!(admission
        .admit(
            &body,
            "POST",
            "/callback",
            UNIX_EPOCH + Duration::from_secs(NOW),
        )
        .is_err());
    assert_eq!(admission.allocation_verifier().calls.borrow().len(), 1);
}
