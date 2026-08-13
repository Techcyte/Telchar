//! Tests nomad callback admission contracts and failure boundaries, including authentication.

mod support;

use std::cell::RefCell;
use std::io;
use std::time::{Duration, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::{Digest, Sha256};
use telchar::backend::BackendKind;
use telchar::nomad_callback::{
    AllocationVerifier, CallbackAdmission, CallbackExecution, CallbackExecutionResolver,
    CallbackResolver, PostgresCallbackExecutionResolver, ReplayAuthority,
};
use telchar::nomad_transfer_authentication::{
    HmacCallbackVerifier, HmacVerificationPolicy, VerifiedHmacRequest,
};
use telchar::nomad_transfer_protocol::{
    Authentication, AuthenticationProof, Direction, Frame, FrameKind, ProtocolLimits,
    encode_metadata, write_frame,
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

#[derive(Default)]
struct RecordingReplayAuthority {
    calls: RefCell<Vec<(String, String)>>,
    accept: bool,
}

impl ReplayAuthority for RecordingReplayAuthority {
    fn reserve(
        &self,
        authentication: &Authentication,
        verified: &VerifiedHmacRequest,
    ) -> io::Result<bool> {
        self.calls.borrow_mut().push((
            authentication.allocation_id.clone(),
            verified.nonce().to_owned(),
        ));
        Ok(self.accept)
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

struct RecordingExecutionResolver {
    execution: Option<CallbackExecution>,
}

impl CallbackExecutionResolver for RecordingExecutionResolver {
    fn resolve(&self, authentication: &Authentication) -> io::Result<Option<CallbackExecution>> {
        if self
            .execution
            .as_ref()
            .is_some_and(|execution| execution.matches(authentication))
        {
            Ok(self.execution.clone())
        } else {
            Ok(None)
        }
    }
}

#[test]
fn resolves_exact_active_callback_execution() {
    let execution = CallbackExecution::new(
        "nomad-primary".to_owned(),
        "telchar".to_owned(),
        "job-1".to_owned(),
        "digest-1".to_owned(),
        "build".to_owned(),
    )
    .expect("execution creates");
    let resolver = CallbackResolver::new(RecordingExecutionResolver {
        execution: Some(execution.clone()),
    });

    assert_eq!(
        resolver
            .resolve(&authentication("request-resolve"))
            .expect("execution resolves"),
        Some(execution)
    );
    let mut foreign = authentication("request-foreign");
    foreign.job_id = "foreign-job".to_owned();
    assert!(
        resolver
            .resolve(&foreign)
            .expect("foreign resolves")
            .is_none()
    );
}

#[test]
fn postgres_resolver_requires_exact_active_nomad_execution() {
    let database = support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("database migrates");
    let digest = [3_u8; 32];
    telchar::persistence::claim_shared_build(
        database.url(),
        "/nix/store/00000000000000000000000000000000-callback.drv",
        &digest,
        "nomad-primary",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("job-1"),
        &["/nix/store/11111111111111111111111111111111-output"],
    )
    .expect("shared build claims");
    telchar::persistence::start_shared_build(
        database.url(),
        "/nix/store/00000000000000000000000000000000-callback.drv",
    )
    .expect("shared build runs");
    let resolver = PostgresCallbackExecutionResolver::new(
        database.url().to_owned(),
        vec![("nomad-primary".to_owned(), "telchar".to_owned())],
    )
    .expect("resolver creates");
    let mut exact = authentication("request-postgres");
    exact.shared_build_digest = URL_SAFE_NO_PAD.encode(Sha256::digest(format!(
        "/nix/store/00000000000000000000000000000000-callback.drv:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )));

    let execution = resolver
        .resolve(&exact)
        .expect("active execution resolves")
        .expect("active execution exists");
    assert_eq!(
        execution.derivation_path(),
        Some("/nix/store/00000000000000000000000000000000-callback.drv")
    );
    let mut foreign = exact.clone();
    foreign.shared_build_digest = "foreign".to_owned();
    assert!(
        resolver
            .resolve(&foreign)
            .expect("foreign execution resolves")
            .is_none()
    );
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
fn requires_replay_reservation_before_allocation_lookup() {
    let allocation = RecordingAllocationVerifier::default();
    let replay = RecordingReplayAuthority::default();
    let mut admission = CallbackAdmission::with_replay(hmac_verifier(), allocation, replay, 4096);

    assert!(
        admission
            .admit(
                &frame(&authentication("request-durable")),
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(NOW),
            )
            .is_err()
    );
    assert!(admission.allocation_verifier().calls.borrow().is_empty());
}

#[test]
fn rejects_invalid_frame_before_allocation_lookup() {
    let allocation = RecordingAllocationVerifier::default();
    let mut admission = CallbackAdmission::new(hmac_verifier(), allocation, 4096);
    let mut body = frame(&authentication("request-2"));
    body.push(0);

    assert!(
        admission
            .admit(
                &body,
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(NOW),
            )
            .is_err()
    );
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

    assert!(
        admission
            .admit(
                &body,
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(NOW),
            )
            .is_err()
    );
    assert!(
        admission
            .admit(
                &body,
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(NOW),
            )
            .is_err()
    );
    assert_eq!(admission.allocation_verifier().calls.borrow().len(), 1);
}
