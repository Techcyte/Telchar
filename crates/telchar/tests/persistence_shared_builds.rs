//! Tests shared-build, build-request, and executor persistence contracts and failure boundaries.

use telchar_test_support::postgres::PostgresFixture;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use telchar::backend::{
    BackendCapabilities, BackendKind, CancellationCapability, ExecutionRecovery, LogRecovery,
};
fn producer_inputs(paths: &[Vec<u8>]) -> Vec<telchar::store::closure::ClosurePath> {
    paths
        .iter()
        .map(|path| telchar::store::closure::ClosurePath {
            store_path: String::from_utf8(path.clone()).unwrap(),
            nar_hash: "sha256:fixture".into(),
            nar_size: 1234,
            references: Vec::new(),
            deriver: None,
            content_address: None,
        })
        .collect()
}

#[test]
fn requester_reference_is_deterministic_and_component_separated() {
    let requester = telchar::service::ipc::RequesterMetadata {
        credential_id: "ssh-pubkey:fixture".into(),
        audit_subject: "fixture".into(),
        quota_subject: "ssh-pubkey:fixture".into(),
    };

    let reference = telchar::persistence::requester_reference(&requester);

    assert_eq!(
        reference,
        "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e"
    );
    assert_eq!(reference.len(), 64);
    assert!(
        reference
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    );
    assert_ne!(
        telchar::persistence::requester_reference(&telchar::service::ipc::RequesterMetadata {
            credential_id: "ab".into(),
            audit_subject: "c".into(),
            quota_subject: "quota".into(),
        }),
        telchar::persistence::requester_reference(&telchar::service::ipc::RequesterMetadata {
            credential_id: "a".into(),
            audit_subject: "bc".into(),
            quota_subject: "quota".into(),
        })
    );
    for requester in [
        telchar::service::ipc::RequesterMetadata {
            credential_id: "other-credential".into(),
            ..requester.clone()
        },
        telchar::service::ipc::RequesterMetadata {
            audit_subject: "other-audit".into(),
            ..requester.clone()
        },
        telchar::service::ipc::RequesterMetadata {
            quota_subject: "other-quota".into(),
            ..requester.clone()
        },
    ] {
        assert_ne!(
            telchar::persistence::requester_reference(&requester),
            reference
        );
    }
}

fn durable_build_request() -> telchar::build::BuildRequest {
    use nix_worker_protocol::{
        ProtocolSessionLimits, WorkerOperation, WorkerReader, write_worker_byte_string,
        write_worker_integer,
    };

    let derivation = b"/nix/store/11111111111111111111111111111111-shared.drv";
    let output = b"/nix/store/22222222222222222222222222222222-shared";
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 36);
    write_worker_byte_string(&mut wire, derivation);
    write_worker_integer(&mut wire, 1);
    write_worker_byte_string(&mut wire, b"out");
    write_worker_byte_string(&mut wire, output);
    write_worker_byte_string(&mut wire, b"r:sha256");
    write_worker_byte_string(
        &mut wire,
        b"0000000000000000000000000000000000000000000000000000000000000000",
    );
    write_worker_integer(&mut wire, 1);
    write_worker_byte_string(
        &mut wire,
        b"/nix/store/33333333333333333333333333333333-input",
    );
    write_worker_byte_string(&mut wire, b"x86_64-linux");
    write_worker_byte_string(&mut wire, b"/bin/sh");
    write_worker_integer(&mut wire, 1);
    write_worker_byte_string(&mut wire, b"-e");
    write_worker_integer(&mut wire, 4);
    for (key, value) in [
        (b"system".as_slice(), b"x86_64-linux".as_slice()),
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"shared".as_slice()),
        (b"out".as_slice(), output.as_slice()),
    ] {
        write_worker_byte_string(&mut wire, key);
        write_worker_byte_string(&mut wire, value);
    }
    write_worker_integer(&mut wire, 0);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );
    assert_eq!(
        reader.read_operation().expect("operation reads"),
        WorkerOperation::BuildDerivation
    );
    let request = reader
        .complete_build_derivation()
        .expect("build request decodes");
    telchar::build::BuildRequest::from_worker_request(
        &request,
        &[telchar::backend::BackendTarget::new(
            "nomad",
            BackendKind::Nomad,
            "x86_64-linux",
            Vec::<String>::new(),
        )
        .expect("backend target creates")],
    )
    .expect("durable build request admits")
}

#[test]
fn nomad_submission_requires_retained_admitted_inputs() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let request = durable_build_request();
    let derivation = std::str::from_utf8(request.derivation_path()).unwrap();
    telchar::persistence::claim_shared_build_with_request(
        fixture.url(),
        derivation,
        &request.shared_build_digest(),
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("retained-input-job"),
        &["/nix/store/22222222222222222222222222222222-shared"],
        &request,
    )
    .unwrap();
    telchar::persistence::start_shared_build(fixture.url(), derivation).unwrap();
    assert!(
        telchar::persistence::retain_nomad_execution_obligation(
            fixture.url(),
            derivation,
            "retained-input-job"
        )
        .is_err()
    );
    telchar::persistence::create_build_request(
        fixture.url(),
        "retained-request",
        derivation,
        "x86_64-linux",
        "audit",
        "quota",
    )
    .unwrap();
    telchar::persistence::create_store_lease(
        fixture.url(),
        "unrelated-input",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "retained-request",
        "/nix/store/44444444444444444444444444444444-unrelated",
        telchar::persistence::StoreLeasePurpose::Input,
    )
    .unwrap();
    assert!(
        telchar::persistence::retain_nomad_execution_obligation(
            fixture.url(),
            derivation,
            "retained-input-job"
        )
        .is_err()
    );
    telchar::persistence::create_store_lease(
        fixture.url(),
        "required-input",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "retained-request",
        "/nix/store/33333333333333333333333333333333-input",
        telchar::persistence::StoreLeasePurpose::Input,
    )
    .unwrap();
    telchar::persistence::retain_nomad_execution_obligation(
        fixture.url(),
        derivation,
        "retained-input-job",
    )
    .unwrap();
}

#[test]
fn reads_authoritative_operational_shared_build_counts() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let request = durable_build_request();
    let digest = request.shared_build_digest();
    let derivation_path = "/nix/store/11111111111111111111111111111111-shared.drv";

    telchar::persistence::claim_shared_build_with_request(
        fixture.url(),
        derivation_path,
        &digest,
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("telchar-build-shared"),
        &["/nix/store/22222222222222222222222222222222-shared"],
        &request,
    )
    .expect("shared build claim succeeds");
    telchar::persistence::enqueue_shared_build(fixture.url(), derivation_path, "subject", 1)
        .expect("shared build enqueues");

    assert_eq!(
        telchar::persistence::read_shared_build_operational_counts(fixture.url())
            .expect("operational counts read"),
        telchar::persistence::SharedBuildOperationalCounts {
            queued: 1,
            running: 0,
            collecting: 0,
        }
    );

    telchar::persistence::start_queued_shared_build(fixture.url(), derivation_path, 1)
        .expect("shared build starts");
    assert_eq!(
        telchar::persistence::read_shared_build_operational_counts(fixture.url())
            .expect("operational counts read"),
        telchar::persistence::SharedBuildOperationalCounts {
            queued: 0,
            running: 1,
            collecting: 0,
        }
    );

    telchar::persistence::collect_shared_build(fixture.url(), derivation_path)
        .expect("shared build collects");
    assert_eq!(
        telchar::persistence::read_shared_build_operational_counts(fixture.url())
            .expect("operational counts read"),
        telchar::persistence::SharedBuildOperationalCounts {
            queued: 0,
            running: 0,
            collecting: 1,
        }
    );
}

#[test]
fn shared_build_persists_exact_admitted_build_request() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let request = durable_build_request();
    let digest = request.shared_build_digest();

    let claim = telchar::persistence::claim_shared_build_with_request(
        fixture.url(),
        "/nix/store/11111111111111111111111111111111-shared.drv",
        &digest,
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("telchar-build-shared"),
        &["/nix/store/22222222222222222222222222222222-shared"],
        &request,
    )
    .expect("shared build claim succeeds");

    assert_eq!(
        claim.build.build_request.as_ref(),
        Some(&request),
        "exact admitted BuildDerivation request persists"
    );
    telchar::persistence::start_shared_build(
        fixture.url(),
        "/nix/store/11111111111111111111111111111111-shared.drv",
    )
    .expect("shared build starts");
    let loaded = telchar::persistence::read_shared_build_by_execution(
        fixture.url(),
        "nomad",
        "telchar-build-shared",
    )
    .expect("shared build reads")
    .expect("shared build exists");
    assert_eq!(loaded.build_request.as_ref(), Some(&request));
    let authority = &loaded
        .build_request
        .as_ref()
        .expect("build request persists")
        .output_authorities()[0];
    assert_eq!(authority.hash_algorithm(), b"r:sha256");
    assert_eq!(
        authority.hash(),
        b"0000000000000000000000000000000000000000000000000000000000000000"
    );
}

#[test]
fn equivalent_shared_build_claims_have_one_owner() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let database_url = fixture.url().to_owned();
    let barrier = Arc::new(std::sync::Barrier::new(3));

    let claims = thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..2 {
            let database_url = database_url.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(scope.spawn(move || {
                barrier.wait();
                telchar::persistence::claim_shared_build(
                    &database_url,
                    "/nix/store/11111111111111111111111111111111-shared.drv",
                    &[7_u8; 32],
                    "ssh-fast",
                    BackendKind::StaticSsh,
                    BackendCapabilities::new(
                        ExecutionRecovery::OutputOnly,
                        CancellationCapability::ConnectionBound,
                        LogRecovery::LiveOnly,
                    ),
                    None,
                    &["/nix/store/22222222222222222222222222222222-shared"],
                )
                .expect("shared build claim succeeds")
            }));
        }
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("claim thread joins"))
            .collect::<Vec<_>>()
    });

    assert_eq!(
        claims
            .iter()
            .filter(|claim| claim.ownership == telchar::persistence::SharedBuildOwnership::Claimed)
            .count(),
        1
    );
    assert_eq!(
        claims
            .iter()
            .filter(|claim| claim.ownership == telchar::persistence::SharedBuildOwnership::Joined)
            .count(),
        1
    );
    assert!(claims.iter().all(|claim| {
        claim.build.derivation_path == "/nix/store/11111111111111111111111111111111-shared.drv"
            && claim.build.state == telchar::persistence::SharedBuildState::Claimed
            && claim.build.backend_name == "ssh-fast"
            && claim.build.capabilities.execution_recovery() == ExecutionRecovery::OutputOnly
            && claim.build.backend_execution_id.is_none()
    }));
}

#[test]
fn running_shared_build_reassigns_exact_backend_before_execution() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation_path = "/nix/store/33333333333333333333333333333333-reassigned.drv";
    let output_path = "/nix/store/44444444444444444444444444444444-reassigned";
    let output_only = BackendKind::StaticSsh.capabilities();

    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[8_u8; 32],
        "ssh-primary",
        BackendKind::StaticSsh,
        output_only,
        None,
        &[output_path],
    )
    .expect("shared build claims");
    telchar::persistence::start_shared_build(fixture.url(), derivation_path)
        .expect("shared build starts");

    let reassigned = telchar::persistence::reassign_running_shared_build(
        fixture.url(),
        derivation_path,
        "nomad-burst",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("telchar-build-reassigned"),
    )
    .expect("running build reassigns");

    assert_eq!(reassigned.backend_name, "nomad-burst");
    assert_eq!(reassigned.backend_kind, BackendKind::Nomad);
    assert_eq!(
        reassigned.backend_execution_id.as_deref(),
        Some("telchar-build-reassigned")
    );
    let attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation_path)
        .expect("attempt reads")
        .expect("attempt exists");
    assert_eq!(attempt.backend_name, "nomad-burst");
    assert_eq!(attempt.backend_kind, BackendKind::Nomad);
    assert_eq!(
        attempt.backend_execution_id.as_deref(),
        Some("telchar-build-reassigned")
    );
    let trace_context = telchar_telemetry::TraceContext::new(
        Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".into()),
        Some("vendor=value".into()),
    )
    .expect("trace context validates");
    telchar::persistence::record_shared_build_attempt_trace_context(
        fixture.url(),
        derivation_path,
        "telchar-build-reassigned",
        &trace_context,
    )
    .expect("attempt trace context records");
    let traced_attempt =
        telchar::persistence::read_shared_build_attempt(fixture.url(), derivation_path)
            .expect("traced attempt reads")
            .expect("traced attempt exists");
    assert_eq!(traced_attempt.trace_context, trace_context);
}

#[test]
fn equivalent_claims_join_despite_provisional_backend_difference() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation_path = "/nix/store/55555555555555555555555555555555-provisional.drv";
    let output_path = "/nix/store/66666666666666666666666666666666-provisional";

    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[9_u8; 32],
        "ssh-primary",
        BackendKind::StaticSsh,
        BackendKind::StaticSsh.capabilities(),
        None,
        &[output_path],
    )
    .expect("first claim succeeds");
    let joined = telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[9_u8; 32],
        "nomad-burst",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("telchar-build-provisional"),
        &[output_path],
    )
    .expect("equivalent claim joins");

    assert_eq!(
        joined.ownership,
        telchar::persistence::SharedBuildOwnership::Joined
    );
    assert_eq!(joined.build.backend_name, "ssh-primary");
}

#[test]
fn shared_build_claim_rejects_digest_conflict_and_requires_adoptable_identity() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation_path = "/nix/store/33333333333333333333333333333333-conflict.drv";
    let output_path = "/nix/store/44444444444444444444444444444444-conflict";
    let output_only = BackendCapabilities::new(
        ExecutionRecovery::OutputOnly,
        CancellationCapability::ConnectionBound,
        LogRecovery::LiveOnly,
    );

    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[1_u8; 32],
        "local",
        BackendKind::Local,
        output_only,
        None,
        &[output_path],
    )
    .expect("first shared build claim succeeds");

    assert_eq!(
        telchar::persistence::claim_shared_build(
            fixture.url(),
            derivation_path,
            &[2_u8; 32],
            "local",
            BackendKind::Local,
            output_only,
            None,
            &[output_path],
        )
        .expect_err("digest disagreement fails closed")
        .failure(),
        telchar::persistence::SharedBuildFailure::Conflict
    );
    assert_eq!(
        telchar::persistence::claim_shared_build(
            fixture.url(),
            "/nix/store/55555555555555555555555555555555-adoptable.drv",
            &[3_u8; 32],
            "nomad",
            BackendKind::Nomad,
            BackendCapabilities::new(
                ExecutionRecovery::Adoptable,
                CancellationCapability::Explicit,
                LogRecovery::LiveOnly,
            ),
            None,
            &["/nix/store/66666666666666666666666666666666-adoptable"],
        )
        .expect_err("adoptable execution requires a stable identity")
        .failure(),
        telchar::persistence::SharedBuildFailure::Configuration
    );
}

#[test]
fn unresolved_nomad_obligation_prevents_attempt_rotation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation = "/nix/store/77777777777777777777777777777777-obligation.drv";
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[4_u8; 32],
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("attempt-1"),
        &["/nix/store/88888888888888888888888888888888-obligation"],
    )
    .expect("claim succeeds");
    telchar::persistence::start_shared_build(fixture.url(), derivation).expect("build starts");
    let attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
        .unwrap()
        .unwrap();
    assert_eq!(
        telchar::persistence::retain_nomad_execution_obligation(
            fixture.url(),
            derivation,
            "attempt-1",
        )
        .expect("execution liability persists before submission"),
        attempt.attempt_id
    );
    assert!(
        telchar::persistence::reassign_running_shared_build(
            fixture.url(),
            derivation,
            "replacement",
            BackendKind::Local,
            BackendKind::Local.capabilities(),
            None,
        )
        .is_err(),
        "unresolved execution cannot migrate backend"
    );
    assert!(
        telchar::persistence::retry_shared_build(
            fixture.url(),
            derivation,
            "attempt-1",
            "attempt-2",
            "nomad-infrastructure-lost",
            &serde_json::json!({}),
        )
        .is_err(),
        "unresolved predecessor cannot be replaced"
    );
    assert_eq!(
        telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
            .unwrap()
            .unwrap(),
        attempt
    );
    telchar::persistence::resolve_nomad_execution_obligation(fixture.url(), attempt.attempt_id)
        .expect("proven absent execution resolves");
    assert!(
        telchar::persistence::retain_nomad_execution_obligation(
            fixture.url(),
            derivation,
            "attempt-1",
        )
        .is_err(),
        "resolved attempt cannot be submitted again"
    );
    assert!(
        telchar::persistence::retry_shared_build(
            fixture.url(),
            derivation,
            "attempt-1",
            "attempt-2",
            "nomad-infrastructure-lost",
            &serde_json::json!({}),
        )
        .is_ok()
    );
}

#[test]
fn unresolved_nomad_obligation_prevents_fresh_claim_after_failure() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let derivation = "/nix/store/77777777777777777777777777777777-liability.drv";
    let outputs = ["/nix/store/88888888888888888888888888888888-liability"];
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[4; 32],
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("liability-job"),
        &outputs,
    )
    .unwrap();
    telchar::persistence::start_shared_build(fixture.url(), derivation).unwrap();
    telchar::persistence::retain_nomad_execution_obligation(
        fixture.url(),
        derivation,
        "liability-job",
    )
    .unwrap();
    telchar::persistence::complete_shared_build_failure(
        fixture.url(),
        derivation,
        "control-plane-unavailable",
        &serde_json::json!({}),
        Duration::from_secs(3600),
    )
    .unwrap();
    let before = telchar::persistence::read_shared_build(fixture.url(), derivation)
        .unwrap()
        .unwrap();
    let claim = telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[4; 32],
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("replacement-job"),
        &outputs,
    )
    .unwrap();
    assert_eq!(
        claim.build, before,
        "unresolved execution cannot be replaced by fresh request"
    );
    assert_eq!(
        claim.ownership,
        telchar::persistence::SharedBuildOwnership::Joined
    );
    let attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
        .unwrap()
        .unwrap();
    telchar::persistence::resolve_nomad_execution_obligation(fixture.url(), attempt.attempt_id)
        .unwrap();
    let reclaimed = telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[4; 32],
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("replacement-job"),
        &outputs,
    )
    .unwrap();
    assert_eq!(
        reclaimed.ownership,
        telchar::persistence::SharedBuildOwnership::Claimed
    );
    assert_eq!(
        reclaimed.build.state,
        telchar::persistence::SharedBuildState::Claimed
    );
}

#[test]
fn predecessor_callback_cannot_complete_successor_attempt() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation = "/nix/store/77777777777777777777777777777777-fenced.drv";
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[4_u8; 32],
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("attempt-1"),
        &["/nix/store/88888888888888888888888888888888-fenced"],
    )
    .expect("claim succeeds");
    telchar::persistence::start_shared_build(fixture.url(), derivation).expect("build starts");
    let predecessor = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
        .expect("attempt reads")
        .expect("attempt exists");
    let successor_attempt = telchar::persistence::retry_shared_build(
        fixture.url(),
        derivation,
        "attempt-1",
        "attempt-2",
        "nomad-infrastructure-lost",
        &serde_json::json!({"reason": "predecessor terminated"}),
    )
    .expect("successor starts");
    assert!(
        telchar::persistence::record_attempt_producer_location(
            fixture.url(),
            derivation,
            predecessor.attempt_id,
            "stale-node",
        )
        .is_err()
    );
    telchar::persistence::record_attempt_producer_location(
        fixture.url(),
        derivation,
        successor_attempt.attempt_id,
        "producer-node",
    )
    .unwrap();
    let successor = telchar::persistence::read_shared_build(fixture.url(), derivation)
        .expect("build reads")
        .expect("build exists");
    assert!(
        telchar::persistence::complete_shared_build_execution(
            fixture.url(),
            derivation,
            predecessor.attempt_id,
            Some("nomad-transfer-failure"),
            &serde_json::json!({"execution_id": "attempt-1"}),
            Duration::from_secs(3_600),
        )
        .is_err(),
        "predecessor callback cannot fail successor"
    );
    assert!(
        telchar::persistence::collect_shared_build_execution(
            fixture.url(),
            derivation,
            predecessor.attempt_id,
        )
        .is_err()
    );
    assert_eq!(
        telchar::persistence::read_shared_build(fixture.url(), derivation)
            .expect("build reads")
            .expect("build exists"),
        successor
    );
    thread::scope(|scope| {
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut callbacks = Vec::new();
        for index in 0..8 {
            let barrier = barrier.clone();
            let database = fixture.url();
            let attempt_id = predecessor.attempt_id;
            callbacks.push(scope.spawn(move || {
                barrier.wait();
                if index % 2 == 0 {
                    telchar::persistence::collect_shared_build_execution(
                        database, derivation, attempt_id,
                    )
                    .is_err()
                } else {
                    telchar::persistence::complete_shared_build_execution(
                        database,
                        derivation,
                        attempt_id,
                        Some("nomad-transfer-failure"),
                        &serde_json::json!({}),
                        Duration::from_secs(3_600),
                    )
                    .is_err()
                }
            }));
        }
        for callback in callbacks {
            assert!(callback.join().expect("stale callback exits"));
        }
    });
    assert_eq!(
        telchar::persistence::read_shared_build(fixture.url(), derivation)
            .expect("successor reads after concurrent callbacks")
            .expect("successor exists"),
        successor
    );
    telchar::persistence::collect_shared_build_execution(
        fixture.url(),
        derivation,
        successor_attempt.attempt_id,
    )
    .expect("successor collects");
    assert!(
        telchar::persistence::complete_shared_build_execution(
            fixture.url(),
            derivation,
            predecessor.attempt_id,
            None,
            &serde_json::json!({"status": "built"}),
            Duration::from_secs(3_600),
        )
        .is_err()
    );
    telchar::persistence::complete_shared_build_execution(
        fixture.url(),
        derivation,
        successor_attempt.attempt_id,
        None,
        &serde_json::json!({"status": "built"}),
        Duration::from_secs(3_600),
    )
    .expect("successor completes");
    let producers = telchar::persistence::read_output_producers(
        fixture.url(),
        &producer_inputs(&[b"/nix/store/88888888888888888888888888888888-fenced".to_vec()]),
    )
    .unwrap();
    assert_eq!(producers[0].location.as_deref(), Some("producer-node"));
    fixture.restart();
    assert_eq!(
        telchar::persistence::read_output_producers(
            fixture.url(),
            &producer_inputs(&[b"/nix/store/88888888888888888888888888888888-fenced".to_vec()]),
        )
        .unwrap(),
        producers
    );
    assert!(
        telchar::persistence::record_attempt_producer_location(
            fixture.url(),
            derivation,
            successor_attempt.attempt_id,
            "terminal-overwrite",
        )
        .is_err()
    );
    let terminal = telchar::persistence::read_shared_build(fixture.url(), derivation).unwrap();
    for attempt in [predecessor.attempt_id, successor_attempt.attempt_id] {
        assert!(
            telchar::persistence::complete_shared_build_execution(
                fixture.url(),
                derivation,
                attempt,
                Some("nomad-transfer-failure"),
                &serde_json::json!({}),
                Duration::from_secs(3_600),
            )
            .is_err()
        );
    }
    assert_eq!(
        telchar::persistence::read_shared_build(fixture.url(), derivation).unwrap(),
        terminal
    );
    fixture.connect().execute(
        "UPDATE shared_builds SET created_at = created_at - interval '2 hours', started_at = started_at - interval '2 hours', collecting_at = collecting_at - interval '2 hours', completed_at = completed_at - interval '2 hours', expires_at = expires_at - interval '2 hours' WHERE derivation_path = $1",
        &[&derivation],
    ).expect("terminal result expires");
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[4_u8; 32],
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("attempt-1"),
        &["/nix/store/88888888888888888888888888888888-fenced"],
    )
    .expect("later request claims reused job identity");
    telchar::persistence::start_shared_build(fixture.url(), derivation)
        .expect("later request starts");
    assert!(
        telchar::persistence::complete_shared_build_execution(
            fixture.url(),
            derivation,
            predecessor.attempt_id,
            Some("nomad-transfer-failure"),
            &serde_json::json!({}),
            Duration::from_secs(3_600),
        )
        .is_err(),
        "attempt identity fences callbacks even when job name is reused"
    );
}

#[test]
fn shared_build_lifecycle_persists_immutable_terminal_success() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation_path = "/nix/store/77777777777777777777777777777777-lifecycle.drv";
    let output_path = "/nix/store/88888888888888888888888888888888-lifecycle";

    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[4_u8; 32],
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("telchar-build-lifecycle"),
        &[output_path],
    )
    .expect("shared build claim succeeds");
    assert_eq!(
        telchar::persistence::start_shared_build(fixture.url(), derivation_path)
            .expect("shared build starts")
            .state,
        telchar::persistence::SharedBuildState::Running
    );
    assert_eq!(
        telchar::persistence::collect_shared_build(fixture.url(), derivation_path)
            .expect("shared build collects")
            .state,
        telchar::persistence::SharedBuildState::Collecting
    );
    let completed = telchar::persistence::complete_shared_build_success(
        fixture.url(),
        derivation_path,
        &serde_json::json!({"status": "built", "output_count": 1}),
        Duration::from_secs(3_600),
    )
    .expect("shared build succeeds");

    assert_eq!(
        completed.state,
        telchar::persistence::SharedBuildState::Succeeded
    );
    assert_eq!(
        completed.result_metadata,
        Some(serde_json::json!({"status": "built", "output_count": 1}))
    );
    assert!(completed.failure_classification.is_none());
    assert!(completed.started_at.is_some());
    assert!(completed.collecting_at.is_some());
    assert!(completed.completed_at.is_some());
    assert!(completed.expires_at.is_some());

    fixture.restart();
    assert_eq!(
        telchar::persistence::read_shared_build(fixture.url(), derivation_path)
            .expect("shared build reads")
            .expect("shared build exists"),
        completed
    );
    assert_eq!(
        telchar::persistence::complete_shared_build_failure(
            fixture.url(),
            derivation_path,
            "infrastructure-failure",
            &serde_json::json!({"stage": "monitor"}),
            Duration::from_secs(3_600),
        )
        .expect_err("terminal shared build is immutable")
        .failure(),
        telchar::persistence::SharedBuildFailure::InvalidState
    );
}

#[test]
fn output_producer_weights_join_each_distinct_retained_path() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let derivation = "/nix/store/77777777777777777777777777777777-weights.drv";
    let outputs = [
        "/nix/store/11111111111111111111111111111111-large",
        "/nix/store/22222222222222222222222222222222-small",
    ];
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[6; 32],
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("weights-job"),
        &outputs,
    )
    .unwrap();
    telchar::persistence::start_shared_build(fixture.url(), derivation).unwrap();
    telchar::persistence::collect_shared_build(fixture.url(), derivation).unwrap();
    telchar::persistence::complete_shared_build_success(
        fixture.url(),
        derivation,
        &serde_json::json!({"status": "built"}),
        Duration::from_secs(3600),
    )
    .unwrap();
    // Reverse query order, include an unproduced path, and duplicate one path.
    let mut inputs = producer_inputs(&[
        outputs[1].as_bytes().to_vec(),
        b"/nix/store/33333333333333333333333333333333-unknown".to_vec(),
        outputs[0].as_bytes().to_vec(),
    ]);
    inputs[0].nar_size = 7;
    inputs[2].nar_size = u64::MAX;
    inputs.push(inputs[2].clone());
    let producers = telchar::persistence::read_output_producers(fixture.url(), &inputs).unwrap();
    assert_eq!(
        producers
            .iter()
            .map(|producer| producer.nar_size)
            .collect::<Vec<_>>(),
        [u64::MAX, 7]
    );
    assert!(
        producers
            .iter()
            .all(|producer| producer.backend_name == "nomad"
                && producer.backend_kind == BackendKind::Nomad)
    );
}

#[test]
fn validated_producer_survives_restart_without_fabricating_cache_production() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let derivation = "/nix/store/77777777777777777777777777777777-producer.drv";
    let output = "/nix/store/33333333333333333333333333333333-input";
    let paths = producer_inputs(&[output.as_bytes().to_vec()]);
    let claim = || {
        telchar::persistence::claim_shared_build(
            fixture.url(),
            derivation,
            &[4; 32],
            "pool.builder",
            BackendKind::StaticSsh,
            BackendKind::StaticSsh.capabilities(),
            Some("ssh-execution"),
            &[output],
        )
        .unwrap()
    };
    claim();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .unwrap();
    let predecessor = owner.database().clone();
    drop(owner);
    let replacement = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(
        predecessor
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM output_producers WHERE store_path = $1",
                &[&output]
            )
            .is_err()
    );
    replacement.verify().unwrap();
    drop(replacement);
    assert!(
        telchar::persistence::read_output_producers(fixture.url(), &paths)
            .unwrap()
            .is_empty()
    );
    telchar::persistence::start_shared_build(fixture.url(), derivation).unwrap();
    telchar::persistence::collect_shared_build(fixture.url(), derivation).unwrap();
    telchar::persistence::complete_shared_build_success(
        fixture.url(),
        derivation,
        &serde_json::json!({"status": "built"}),
        Duration::from_secs(3600),
    )
    .unwrap();
    let producers = telchar::persistence::read_output_producers(fixture.url(), &paths).unwrap();
    assert_eq!(
        producers,
        vec![telchar::persistence::OutputProducer {
            backend_name: "pool.builder".into(),
            backend_kind: BackendKind::StaticSsh,
            location: None,
            nar_size: 1234,
        }]
    );
    fixture.restart();
    assert_eq!(
        telchar::persistence::read_output_producers(fixture.url(), &paths).unwrap(),
        producers
    );
    let mut resized_inputs = paths.clone();
    resized_inputs[0].nar_size = 9876;
    resized_inputs.push(resized_inputs[0].clone());
    let resized =
        telchar::persistence::read_output_producers(fixture.url(), &resized_inputs).unwrap();
    assert_eq!(resized.len(), 1, "one size per distinct producer path");
    assert_eq!(
        resized[0].nar_size, 9876,
        "size comes from this retained closure, not persistence"
    );
    assert!(
        telchar::persistence::read_output_producers(fixture.url(), &[])
            .unwrap()
            .is_empty()
    );
    let root = tempfile::tempdir().unwrap();
    let identity = root.path().join("identity");
    let known_hosts = root.path().join("known-hosts");
    std::fs::write(&identity, "fixture-key").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&known_hosts, "builder ssh-ed25519 AAAA\n").unwrap();
    let ssh = root.path().join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config_path = root.path().join("service.toml");
    std::fs::write(&config_path, format!(
        "[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n[[backends.ssh]]\nidentity_file = {:?}\nknown_hosts_file = {:?}\nssh_program = {:?}\n[backends.ssh.pool]\nsource = \"static\"\n[backends.ssh.pool.builder]\naddress = \"builder\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n", identity, known_hosts, ssh,
    )).unwrap();
    let config =
        telchar::service::config::ServiceConfig::load_with_environment(&config_path, |_| None)
            .unwrap();
    let inventory = config.static_ssh_backends().to_vec();
    let health = telchar::backend::static_ssh::StaticSshHealth::from_states(
        &inventory,
        [(
            "pool.builder",
            telchar::backend::static_ssh::StaticSshHealthState::Ready,
        )],
    );
    let backends =
        telchar::backend::routing::ConfiguredBackends::with_health(&config, None, None, health)
            .unwrap();
    let executor = backends
        .executor(
            telchar::persistence::Database::connect(fixture.url()).unwrap(),
            Arc::new(telchar::shared_build::SharedBuildRegistry::new()),
        )
        .unwrap();
    let request = durable_build_request();
    let mut execution =
        telchar::backend::BuildExecution::new("consumer", &request, Duration::from_secs(60))
            .unwrap();
    execution.set_producers(
        telchar::persistence::read_output_producers(
            fixture.url(),
            &producer_inputs(request.input_sources()),
        )
        .unwrap(),
    );
    use telchar::backend::BuildBackend;
    let permit = executor.reserve_build(&execution, &[]).unwrap().unwrap();
    assert_eq!(permit.target().name(), "pool.builder");
    let fallback = executor.reserve_build(&execution, &[]).unwrap().unwrap();
    assert_eq!(fallback.target().name(), "local");
    let cached = "/nix/store/99999999999999999999999999999999-cached.drv";
    let cached_output = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-cached";
    telchar::persistence::claim_shared_build(
        fixture.url(),
        cached,
        &[5; 32],
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("unused-job"),
        &[cached_output],
    )
    .unwrap();
    telchar::persistence::start_shared_build(fixture.url(), cached).unwrap();
    telchar::persistence::collect_shared_build(fixture.url(), cached).unwrap();
    telchar::persistence::complete_shared_build_success(
        fixture.url(),
        cached,
        &serde_json::json!({"status": "already-valid"}),
        Duration::from_secs(3600),
    )
    .unwrap();
    assert!(
        telchar::persistence::read_output_producers(
            fixture.url(),
            &producer_inputs(&[cached_output.as_bytes().to_vec()]),
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn shared_build_lifecycle_rejects_skipped_transitions_and_bounds_terminal_data() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation_path = "/nix/store/99999999999999999999999999999999-lifecycle.drv";

    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[5_u8; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-lifecycle"],
    )
    .expect("shared build claim succeeds");

    assert_eq!(
        telchar::persistence::collect_shared_build(fixture.url(), derivation_path)
            .expect_err("claimed build cannot collect")
            .failure(),
        telchar::persistence::SharedBuildFailure::InvalidState
    );
    telchar::persistence::start_shared_build(fixture.url(), derivation_path)
        .expect("shared build starts");
    assert_eq!(
        telchar::persistence::complete_shared_build_success(
            fixture.url(),
            derivation_path,
            &serde_json::json!({"status": "built"}),
            Duration::from_secs(3_600),
        )
        .expect_err("running build cannot skip collection")
        .failure(),
        telchar::persistence::SharedBuildFailure::InvalidState
    );
    assert_eq!(
        telchar::persistence::complete_shared_build_failure(
            fixture.url(),
            derivation_path,
            "",
            &serde_json::json!({}),
            Duration::from_secs(3_600),
        )
        .expect_err("empty classification rejects")
        .failure(),
        telchar::persistence::SharedBuildFailure::Configuration
    );
    assert_eq!(
        telchar::persistence::complete_shared_build_failure(
            fixture.url(),
            derivation_path,
            "build-failure",
            &serde_json::json!([]),
            Duration::from_secs(3_600),
        )
        .expect_err("non-object result rejects")
        .failure(),
        telchar::persistence::SharedBuildFailure::Configuration
    );
}

#[test]
fn expired_success_can_be_claimed_for_execution() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-expired.drv";
    let output_path = "/nix/store/cccccccccccccccccccccccccccccccc-expired";
    let claim = || {
        telchar::persistence::claim_shared_build(
            fixture.url(),
            derivation_path,
            &[6_u8; 32],
            "local",
            BackendKind::Local,
            BackendKind::Local.capabilities(),
            None,
            &[output_path],
        )
        .expect("shared build claims")
    };
    claim();
    telchar::persistence::start_shared_build(fixture.url(), derivation_path).unwrap();
    telchar::persistence::collect_shared_build(fixture.url(), derivation_path).unwrap();
    telchar::persistence::complete_shared_build_success(
        fixture.url(),
        derivation_path,
        &serde_json::json!({"status": "built", "output_count": 1}),
        Duration::from_secs(3_600),
    )
    .unwrap();
    assert_eq!(
        claim().ownership,
        telchar::persistence::SharedBuildOwnership::Joined
    );
    fixture.connect().execute(
        "UPDATE shared_builds SET created_at = created_at - interval '2 hours', started_at = started_at - interval '2 hours', collecting_at = collecting_at - interval '2 hours', completed_at = completed_at - interval '2 hours', expires_at = expires_at - interval '2 hours' WHERE derivation_path = $1",
        &[&derivation_path],
    ).unwrap();
    let replacement = claim();
    assert_eq!(
        replacement.ownership,
        telchar::persistence::SharedBuildOwnership::Claimed
    );
    assert_eq!(
        replacement.build.state,
        telchar::persistence::SharedBuildState::Claimed
    );
    assert!(replacement.build.result_metadata.is_none());
    assert!(replacement.build.expires_at.is_none());
}

#[test]
fn later_request_replaces_failed_shared_build_without_automatic_retry() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-retry.drv";
    let output_path = "/nix/store/cccccccccccccccccccccccccccccccc-retry";

    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[6_u8; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &[output_path],
    )
    .expect("first shared build claims");
    telchar::persistence::enqueue_shared_build(fixture.url(), derivation_path, "alice", 1)
        .expect("first shared build enqueues");
    telchar::persistence::complete_shared_build_failure(
        fixture.url(),
        derivation_path,
        "infrastructure-failure",
        &serde_json::json!({"stage": "execute"}),
        Duration::from_secs(3_600),
    )
    .expect("first shared build fails");

    let replacement = telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[7_u8; 32],
        "ssh-fast",
        BackendKind::StaticSsh,
        BackendKind::StaticSsh.capabilities(),
        None,
        &[output_path],
    )
    .expect("later independent request claims a replacement");

    assert_eq!(
        replacement.ownership,
        telchar::persistence::SharedBuildOwnership::Claimed
    );
    assert_eq!(
        replacement.build.state,
        telchar::persistence::SharedBuildState::Claimed
    );
    assert_eq!(replacement.build.request_digest, [7_u8; 32]);
    assert_eq!(replacement.build.backend_name, "ssh-fast");
    assert!(replacement.build.result_metadata.is_none());
    assert!(replacement.build.completed_at.is_none());
    telchar::persistence::enqueue_shared_build(fixture.url(), derivation_path, "alice", 2)
        .expect("replacement shared build enqueues");
    assert_eq!(
        fixture
            .connect()
            .query_one(
                "SELECT count(*) FROM shared_builds WHERE derivation_path = $1",
                &[&derivation_path],
            )
            .expect("shared build count reads")
            .get::<_, i64>(0),
        1
    );
}

#[test]
fn active_shared_builds_survive_restart_in_deterministic_order() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    for (derivation_path, digest) in [
        (
            "/nix/store/dddddddddddddddddddddddddddddddd-active-a.drv",
            [8_u8; 32],
        ),
        (
            "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-active-b.drv",
            [9_u8; 32],
        ),
    ] {
        telchar::persistence::claim_shared_build(
            fixture.url(),
            derivation_path,
            &digest,
            "nomad",
            BackendKind::Nomad,
            BackendKind::Nomad.capabilities(),
            Some(if digest[0] == 8 { "nomad-a" } else { "nomad-b" }),
            &["/nix/store/ffffffffffffffffffffffffffffffff-active"],
        )
        .expect("active shared build claims");
    }
    telchar::persistence::start_shared_build(
        fixture.url(),
        "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-active-b.drv",
    )
    .expect("second shared build starts");

    fixture.restart();
    let active = telchar::persistence::read_active_shared_builds(fixture.url(), 16)
        .expect("active shared builds read");

    assert_eq!(active.len(), 2);
    assert!(active[0].created_at <= active[1].created_at);
    assert_eq!(
        active.iter().map(|build| build.state).collect::<Vec<_>>(),
        [
            telchar::persistence::SharedBuildState::Claimed,
            telchar::persistence::SharedBuildState::Running,
        ]
    );
    assert_eq!(
        telchar::persistence::read_active_shared_builds(fixture.url(), 0)
            .expect_err("zero active-build limit rejects")
            .failure(),
        telchar::persistence::SharedBuildFailure::Configuration
    );
}
