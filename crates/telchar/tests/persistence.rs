mod support;

use std::fmt;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use postgres::types::Type;
use sha2::{Digest, Sha256};
use support::postgres::PostgresFixture;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

#[test]
fn requester_reference_is_deterministic_and_component_separated() {
    let requester = telchar::ipc::RequesterMetadata {
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
    assert!(reference
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
    assert_ne!(
        telchar::persistence::requester_reference(&telchar::ipc::RequesterMetadata {
            credential_id: "ab".into(),
            audit_subject: "c".into(),
            quota_subject: "quota".into(),
        }),
        telchar::persistence::requester_reference(&telchar::ipc::RequesterMetadata {
            credential_id: "a".into(),
            audit_subject: "bc".into(),
            quota_subject: "quota".into(),
        })
    );
    for requester in [
        telchar::ipc::RequesterMetadata {
            credential_id: "other-credential".into(),
            ..requester.clone()
        },
        telchar::ipc::RequesterMetadata {
            audit_subject: "other-audit".into(),
            ..requester.clone()
        },
        telchar::ipc::RequesterMetadata {
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

#[test]
fn open_and_read_protocol_session_persist_requested_state() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";

    let opened = telchar::persistence::open_protocol_session(
        fixture.url(),
        "session-1",
        requester_reference,
        "release-engineering",
        "build-farm",
    )
    .expect("session opens");
    let read = telchar::persistence::read_protocol_session(fixture.url(), "session-1")
        .expect("session reads")
        .expect("session exists");

    assert_eq!(opened, read);
    assert_eq!(read.session_id, "session-1");
    assert_eq!(read.requester_reference, requester_reference);
    assert_eq!(read.audit_subject, "release-engineering");
    assert_eq!(read.quota_subject, "build-farm");
    assert_eq!(read.state, telchar::persistence::ProtocolSessionState::Open);
    assert!(read.closed_at.is_none());

    let closed = telchar::persistence::close_protocol_session(fixture.url(), "session-1")
        .expect("session closes");
    assert_eq!(closed.audit_subject, "release-engineering");
    assert_eq!(closed.quota_subject, "build-farm");
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.url(), "session-1")
            .expect("closed session reads"),
        Some(closed)
    );
}

#[test]
fn protocol_session_operation_rejects_unbounded_audit_metadata_before_connection() {
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    let long_audit_subject = "a".repeat(telchar::ipc::MAX_IPC_COMPONENT_BYTES + 1);
    let long_quota_subject = "q".repeat(telchar::ipc::MAX_IPC_CREDENTIAL_ID_BYTES + 1);
    for (audit_subject, quota_subject) in [
        ("", "quota"),
        ("audit", ""),
        (long_audit_subject.as_str(), "quota"),
        ("audit", long_quota_subject.as_str()),
    ] {
        assert_eq!(
            telchar::persistence::open_protocol_session(
                "postgresql://127.0.0.1:1/no-connection",
                "bounded-session",
                requester_reference,
                audit_subject,
                quota_subject,
            )
            .expect_err("invalid metadata rejects before connection")
            .failure(),
            telchar::persistence::ProtocolSessionFailure::Configuration
        );
    }
}

#[test]
fn create_and_read_build_request_persist_immutable_state() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "request-session",
        "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e",
        "release-engineering",
        "build-farm",
    )
    .expect("session opens");

    let created = telchar::persistence::create_build_request(
        fixture.url(),
        "request-1",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "release-engineering",
        "build-farm",
    )
    .expect("build request persists");
    let read = telchar::persistence::read_build_request(fixture.url(), "request-1")
        .expect("build request reads")
        .expect("build request exists");

    assert_eq!(created, read);
    assert_eq!(read.request_id, "request-1");
    assert_eq!(
        read.derivation_path,
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv"
    );
    assert_eq!(read.system, "x86_64-linux");
    assert_eq!(
        read.queue_state,
        telchar::persistence::BuildQueueState::Accepted
    );
    assert!(read.queued_at.is_none());
    assert_eq!(read.audit_subject, "release-engineering");
    assert_eq!(read.quota_subject, "build-farm");
    assert!(
        telchar::persistence::read_build_request(fixture.url(), "absent-request")
            .expect("absent request reads")
            .is_none()
    );
}

#[test]
fn build_request_operation_rejects_unbounded_subjects_before_connection() {
    let path = "/nix/store/11111111111111111111111111111111-bounded-request.drv";
    let long_audit_subject = "a".repeat(telchar::ipc::MAX_IPC_COMPONENT_BYTES + 1);
    let long_quota_subject = "q".repeat(telchar::ipc::MAX_IPC_CREDENTIAL_ID_BYTES + 1);
    for (audit_subject, quota_subject) in [
        ("", "quota"),
        ("audit", ""),
        (long_audit_subject.as_str(), "quota"),
        ("audit", long_quota_subject.as_str()),
    ] {
        assert_eq!(
            telchar::persistence::create_build_request(
                "postgresql://127.0.0.1:1/no-connection",
                "bounded-request",
                path,
                "x86_64-linux",
                audit_subject,
                quota_subject,
            )
            .expect_err("invalid metadata rejects before connection")
            .failure(),
            telchar::persistence::BuildRequestFailure::Configuration
        );
    }
}

#[test]
fn accepted_request_queues_only_after_required_leases_are_durable() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let request = telchar::persistence::create_build_request(
        fixture.url(),
        "queue-request",
        "/nix/store/11111111111111111111111111111111-queue.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");

    assert_eq!(
        telchar::persistence::queue_build_request(fixture.url(), "queue-request")
            .expect_err("request without leases cannot queue")
            .failure(),
        telchar::persistence::BuildRequestFailure::InvalidState
    );
    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), "queue-request")
            .expect("request reads"),
        Some(request)
    );

    telchar::persistence::create_store_lease(
        fixture.url(),
        "queue-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "queue-request",
        "/nix/store/11111111111111111111111111111111-queue.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "queue-request",
        &[(
            "queue-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-input".to_owned(),
        )],
    )
    .expect("input lease persists");

    let queued = telchar::persistence::queue_build_request(fixture.url(), "queue-request")
        .expect("request queues");
    assert_eq!(
        queued.queue_state,
        telchar::persistence::BuildQueueState::Queued
    );
    assert!(queued
        .queued_at
        .is_some_and(|queued_at| queued_at >= queued.created_at));
    assert_eq!(
        telchar::persistence::queue_build_request(fixture.url(), "queue-request")
            .expect_err("request cannot queue twice")
            .failure(),
        telchar::persistence::BuildRequestFailure::InvalidState
    );

    telchar::persistence::create_build_request(
        fixture.url(),
        "queue-rollback-request",
        "/nix/store/33333333333333333333333333333333-queue-rollback.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("rollback request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "queue-rollback-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "queue-rollback-request",
        "/nix/store/33333333333333333333333333333333-queue-rollback.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("rollback derivation lease persists");
    assert_eq!(
        telchar::persistence::queue_build_request(fixture.url(), "queue-rollback-request")
            .expect_err("partial lease set cannot queue")
            .failure(),
        telchar::persistence::BuildRequestFailure::InvalidState
    );
    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), "queue-rollback-request")
            .expect("rollback request reads")
            .expect("rollback request exists")
            .queue_state,
        telchar::persistence::BuildQueueState::Accepted
    );
}

#[test]
fn local_backend_execution_registry_is_idempotent_and_survives_restart() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let specification_digest = [7_u8; 32];

    let created = telchar::persistence::register_local_backend_execution(
        fixture.url(),
        "local-execution-1",
        "request-1:1",
        &specification_digest,
    )
    .expect("execution registers");
    let repeated = telchar::persistence::register_local_backend_execution(
        fixture.url(),
        "local-execution-1",
        "request-1:1",
        &specification_digest,
    )
    .expect("exact duplicate is idempotent");

    assert_eq!(created, repeated);
    assert_eq!(created.backend_execution_id, "local-execution-1");
    assert_eq!(created.idempotency_key, "request-1:1");
    assert_eq!(created.specification_digest, specification_digest);
    assert_eq!(
        created.state,
        telchar::persistence::LocalBackendExecutionState::Accepted
    );
    assert!(created.started_at.is_none());
    assert!(created.completed_at.is_none());

    for (backend_execution_id, idempotency_key, digest) in [
        ("local-execution-1", "request-1:1", [8_u8; 32]),
        ("local-execution-1", "other-request:1", specification_digest),
        ("other-execution", "request-1:1", specification_digest),
    ] {
        assert_eq!(
            telchar::persistence::register_local_backend_execution(
                fixture.url(),
                backend_execution_id,
                idempotency_key,
                &digest,
            )
            .expect_err("conflicting identity rejects")
            .failure(),
            telchar::persistence::LocalBackendExecutionFailure::Conflict
        );
    }

    fixture.restart();
    assert_eq!(
        telchar::persistence::read_local_backend_execution(fixture.url(), "local-execution-1",)
            .expect("execution reads after restart"),
        Some(created)
    );
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM local_backend_executions", &[])
            .expect("registry count reads")
            .get::<_, i64>(0),
        1
    );
}

#[test]
fn queued_request_dispatches_once_with_attempt_and_capacity_reservation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "dispatch-request",
        "/nix/store/11111111111111111111111111111111-dispatch.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "dispatch-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "dispatch-request",
        "/nix/store/11111111111111111111111111111111-dispatch.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "dispatch-request",
        &[(
            "dispatch-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::queue_build_request(fixture.url(), "dispatch-request")
        .expect("request queues");

    let database_url = fixture.url().to_owned();
    let first = std::thread::spawn({
        let database_url = database_url.clone();
        move || {
            telchar::persistence::dispatch_build_request(
                &database_url,
                "dispatch-request",
                "dispatch-attempt-1",
                1,
                "dispatch-request:1",
                "local",
                "dispatch-reservation-1",
                1,
            )
        }
    });
    let second = std::thread::spawn(move || {
        telchar::persistence::dispatch_build_request(
            &database_url,
            "dispatch-request",
            "dispatch-attempt-2",
            2,
            "dispatch-request:2",
            "local",
            "dispatch-reservation-2",
            1,
        )
    });
    let results = [
        first.join().expect("first dispatcher joins"),
        second.join().expect("second dispatcher joins"),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(|error| error.failure())
            .collect::<Vec<_>>(),
        vec![telchar::persistence::ExecutionAttemptFailure::InvalidState]
    );

    let dispatched = results
        .into_iter()
        .find_map(Result::ok)
        .expect("one dispatch wins");
    assert_eq!(
        dispatched.request.queue_state,
        telchar::persistence::BuildQueueState::Dispatching
    );
    assert_eq!(
        dispatched.attempt.state,
        telchar::persistence::ExecutionAttemptState::Dispatching
    );
    assert_eq!(
        dispatched.reservation.phase,
        telchar::persistence::CapacityReservationPhase::Dispatching
    );
    assert_eq!(
        dispatched.reservation.attempt_id,
        dispatched.attempt.attempt_id
    );
    assert_eq!(dispatched.reservation.quota_subject, "test-quota");
    assert_eq!(dispatched.reservation.units, 1);
    assert!(dispatched.reservation.released_at.is_none());

    let mut client = fixture.connect();
    for table in ["execution_attempts", "capacity_reservations"] {
        assert_eq!(
            client
                .query_one(&format!("SELECT count(*) FROM {table}"), &[])
                .expect("row count reads")
                .get::<_, i64>(0),
            1
        );
    }
}

#[test]
fn dispatching_attempt_recovery_fences_ambiguous_submission_atomically() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "recovery-request",
        "/nix/store/11111111111111111111111111111111-recovery.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "recovery-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "recovery-request",
        "/nix/store/11111111111111111111111111111111-recovery.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "recovery-request",
        &[(
            "recovery-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::queue_build_request(fixture.url(), "recovery-request")
        .expect("request queues");
    let dispatched = telchar::persistence::dispatch_build_request(
        fixture.url(),
        "recovery-request",
        "recovery-attempt",
        1,
        "recovery-request:1",
        "local",
        "recovery-reservation",
        2,
    )
    .expect("request dispatches");

    fixture.restart();
    let recovered = telchar::persistence::recover_dispatching_attempts(fixture.url(), 256)
        .expect("dispatch recovery succeeds");

    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].attempt.attempt_id, "recovery-attempt");
    assert_eq!(recovered[0].attempt.idempotency_key, "recovery-request:1");
    assert_eq!(recovered[0].attempt.backend_execution_id, None);
    assert_eq!(
        recovered[0].attempt.state,
        telchar::persistence::ExecutionAttemptState::Reconciling
    );
    assert!(recovered[0].attempt.fenced_at.is_some());
    assert_eq!(
        recovered[0].request.queue_state,
        telchar::persistence::BuildQueueState::Reconciling
    );
    assert_eq!(
        recovered[0].reservation.reservation_id,
        dispatched.reservation.reservation_id
    );
    assert_eq!(
        recovered[0].reservation.attempt_id,
        dispatched.reservation.attempt_id
    );
    assert_eq!(recovered[0].reservation.phase, dispatched.reservation.phase);
    assert_eq!(recovered[0].reservation.units, dispatched.reservation.units);
    assert!(recovered[0].reservation.released_at.is_some());

    assert!(
        telchar::persistence::recover_dispatching_attempts(fixture.url(), 256)
            .expect("repeated recovery succeeds")
            .is_empty()
    );
    assert_eq!(
        telchar::persistence::record_backend_submission(
            fixture.url(),
            "recovery-attempt",
            "late-backend-execution",
        )
        .expect_err("fenced attempt rejects late submission")
        .failure(),
        telchar::persistence::ExecutionAttemptFailure::InvalidState
    );
}

#[test]
fn local_backend_execution_transitions_to_running_once() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let accepted = telchar::persistence::register_local_backend_execution(
        fixture.url(),
        "local-running-transition",
        "running-transition:1",
        &[5_u8; 32],
    )
    .expect("backend execution persists");

    let running = telchar::persistence::record_local_backend_running(
        fixture.url(),
        "local-running-transition",
    )
    .expect("backend execution starts");

    assert_eq!(
        running.state,
        telchar::persistence::LocalBackendExecutionState::Running
    );
    assert!(running
        .started_at
        .is_some_and(|started_at| started_at >= accepted.created_at));
    assert!(running.completed_at.is_none());
    assert_eq!(
        telchar::persistence::record_local_backend_running(
            fixture.url(),
            "local-running-transition"
        )
        .expect_err("running transition is immutable")
        .failure(),
        telchar::persistence::LocalBackendExecutionFailure::InvalidState
    );
}

#[test]
fn backend_pending_attempt_is_recovered_without_new_attempt_or_submission() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "pending-recovery-request",
        "/nix/store/11111111111111111111111111111111-pending-recovery.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "pending-recovery-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "pending-recovery-request",
        "/nix/store/11111111111111111111111111111111-pending-recovery.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "pending-recovery-request",
        &[(
            "pending-recovery-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::queue_build_request(fixture.url(), "pending-recovery-request")
        .expect("request queues");
    telchar::persistence::dispatch_build_request(
        fixture.url(),
        "pending-recovery-request",
        "pending-recovery-attempt",
        1,
        "pending-recovery-request:1",
        "local",
        "pending-recovery-reservation",
        1,
    )
    .expect("request dispatches");
    telchar::persistence::register_local_backend_execution(
        fixture.url(),
        "local-pending-recovery",
        "pending-recovery-request:1",
        &[7_u8; 32],
    )
    .expect("backend execution persists");
    let submitted = telchar::persistence::record_backend_submission(
        fixture.url(),
        "pending-recovery-attempt",
        "local-pending-recovery",
    )
    .expect("backend submission persists");

    fixture.restart();
    let recovered = telchar::persistence::recover_backend_pending_attempts(fixture.url(), 256)
        .expect("pending recovery succeeds");

    assert_eq!(recovered, vec![submitted.clone()]);
    assert_eq!(
        telchar::persistence::recover_backend_pending_attempts(fixture.url(), 256)
            .expect("repeated recovery succeeds"),
        vec![submitted]
    );
    let mut client = fixture.connect();
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM execution_attempts", &[])
            .expect("attempt count reads")
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM local_backend_executions", &[])
            .expect("backend count reads")
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM execution_attempts WHERE attempt_id = 'pending-recovery-attempt' AND idempotency_key = 'pending-recovery-request:1' AND backend_execution_id = 'local-pending-recovery' AND state = 'backend-pending'",
                &[],
            )
            .expect("stable attempt reads")
            .get::<_, i64>(0),
        1
    );
}

#[test]
fn dispatching_attempt_records_backend_submission_atomically() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "submission-request",
        "/nix/store/11111111111111111111111111111111-submission.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "submission-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "submission-request",
        "/nix/store/11111111111111111111111111111111-submission.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "submission-request",
        &[(
            "submission-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::queue_build_request(fixture.url(), "submission-request")
        .expect("request queues");
    let dispatched = telchar::persistence::dispatch_build_request(
        fixture.url(),
        "submission-request",
        "submission-attempt",
        1,
        "submission-request:1",
        "local",
        "submission-reservation",
        1,
    )
    .expect("request dispatches");

    let submitted = telchar::persistence::record_backend_submission(
        fixture.url(),
        "submission-attempt",
        "backend-execution-1",
    )
    .expect("backend submission persists");
    assert_eq!(
        submitted.request.queue_state,
        telchar::persistence::BuildQueueState::BackendPending
    );
    assert_eq!(
        submitted.attempt.state,
        telchar::persistence::ExecutionAttemptState::BackendPending
    );
    assert_eq!(
        submitted.attempt.backend_execution_id.as_deref(),
        Some("backend-execution-1")
    );
    assert!(submitted
        .attempt
        .submitted_at
        .is_some_and(|submitted_at| submitted_at >= dispatched.attempt.created_at));
    assert_eq!(
        submitted.reservation.phase,
        telchar::persistence::CapacityReservationPhase::BackendPending
    );
    assert_eq!(submitted.reservation.attempt_id, "submission-attempt");
    assert!(submitted.reservation.released_at.is_none());

    assert_eq!(
        telchar::persistence::record_backend_submission(
            fixture.url(),
            "submission-attempt",
            "backend-execution-2",
        )
        .expect_err("submission cannot be replaced")
        .failure(),
        telchar::persistence::ExecutionAttemptFailure::InvalidState
    );
    assert_eq!(
        telchar::persistence::read_execution_attempt(fixture.url(), "submission-attempt")
            .expect("attempt reads")
            .expect("attempt exists"),
        submitted.attempt
    );

    let mut client = fixture.connect();
    client
        .execute(
            "UPDATE capacity_reservations SET released_at = transaction_timestamp() WHERE reservation_id = 'submission-reservation'",
            &[],
        )
        .expect("reservation releases for fault fixture");
    client
        .execute(
            "UPDATE build_requests SET queue_state = 'dispatching' WHERE request_id = 'submission-request'",
            &[],
        )
        .expect("request resets for fault fixture");
    client
        .execute(
            "UPDATE execution_attempts SET state = 'dispatching', backend_execution_id = NULL, submitted_at = NULL WHERE attempt_id = 'submission-attempt'",
            &[],
        )
        .expect("attempt resets for fault fixture");
    assert_eq!(
        telchar::persistence::record_backend_submission(
            fixture.url(),
            "submission-attempt",
            "backend-execution-3",
        )
        .expect_err("missing active reservation rejects transition")
        .failure(),
        telchar::persistence::ExecutionAttemptFailure::InvalidState
    );
    assert_eq!(
        telchar::persistence::read_execution_attempt(fixture.url(), "submission-attempt")
            .expect("attempt reads")
            .expect("attempt exists")
            .state,
        telchar::persistence::ExecutionAttemptState::Dispatching
    );
    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), "submission-request")
            .expect("request reads")
            .expect("request exists")
            .queue_state,
        telchar::persistence::BuildQueueState::Dispatching
    );
}

#[test]
fn backend_pending_attempt_transitions_to_running_atomically() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "running-request",
        "/nix/store/11111111111111111111111111111111-running.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "running-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "running-request",
        "/nix/store/11111111111111111111111111111111-running.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "running-request",
        &[(
            "running-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::queue_build_request(fixture.url(), "running-request")
        .expect("request queues");
    telchar::persistence::dispatch_build_request(
        fixture.url(),
        "running-request",
        "running-attempt",
        1,
        "running-request:1",
        "local",
        "running-reservation",
        2,
    )
    .expect("request dispatches");
    let submitted = telchar::persistence::record_backend_submission(
        fixture.url(),
        "running-attempt",
        "backend-execution-running",
    )
    .expect("submission persists");

    let running = telchar::persistence::record_backend_running(fixture.url(), "running-attempt")
        .expect("running state persists");
    assert_eq!(
        running.request.queue_state,
        telchar::persistence::BuildQueueState::Running
    );
    assert_eq!(
        running.attempt.state,
        telchar::persistence::ExecutionAttemptState::Running
    );
    assert_eq!(
        running.attempt.backend_execution_id,
        submitted.attempt.backend_execution_id
    );
    assert!(running.attempt.started_at.is_some_and(|started_at| {
        submitted
            .attempt
            .submitted_at
            .is_some_and(|submitted_at| started_at >= submitted_at)
    }));
    assert_eq!(
        running.reservation.phase,
        telchar::persistence::CapacityReservationPhase::Running
    );
    assert_eq!(running.reservation.units, 2);
    assert!(running.reservation.released_at.is_none());

    assert_eq!(
        telchar::persistence::record_backend_running(fixture.url(), "running-attempt")
            .expect_err("running transition cannot repeat")
            .failure(),
        telchar::persistence::ExecutionAttemptFailure::InvalidState
    );

    let mut client = fixture.connect();
    client
        .execute(
            "UPDATE capacity_reservations SET released_at = transaction_timestamp() WHERE reservation_id = 'running-reservation'",
            &[],
        )
        .expect("reservation releases for fault fixture");
    client
        .execute(
            "UPDATE build_requests SET queue_state = 'backend-pending' WHERE request_id = 'running-request'",
            &[],
        )
        .expect("request resets for fault fixture");
    client
        .execute(
            "UPDATE execution_attempts SET state = 'backend-pending', started_at = NULL WHERE attempt_id = 'running-attempt'",
            &[],
        )
        .expect("attempt resets for fault fixture");
    assert_eq!(
        telchar::persistence::record_backend_running(fixture.url(), "running-attempt")
            .expect_err("missing reservation rejects running transition")
            .failure(),
        telchar::persistence::ExecutionAttemptFailure::InvalidState
    );
    let attempt = telchar::persistence::read_execution_attempt(fixture.url(), "running-attempt")
        .expect("attempt reads")
        .expect("attempt exists");
    assert_eq!(
        attempt.state,
        telchar::persistence::ExecutionAttemptState::BackendPending
    );
    assert!(attempt.started_at.is_none());
    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), "running-request")
            .expect("request reads")
            .expect("request exists")
            .queue_state,
        telchar::persistence::BuildQueueState::BackendPending
    );
}

#[test]
fn running_attempt_transitions_to_collecting_without_terminal_success() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "collecting-request",
        "/nix/store/11111111111111111111111111111111-collecting.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "collecting-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "collecting-request",
        "/nix/store/11111111111111111111111111111111-collecting.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "collecting-request",
        &[(
            "collecting-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::queue_build_request(fixture.url(), "collecting-request")
        .expect("request queues");
    telchar::persistence::dispatch_build_request(
        fixture.url(),
        "collecting-request",
        "collecting-attempt",
        1,
        "collecting-request:1",
        "local",
        "collecting-reservation",
        2,
    )
    .expect("request dispatches");
    telchar::persistence::record_backend_submission(
        fixture.url(),
        "collecting-attempt",
        "backend-execution-collecting",
    )
    .expect("submission persists");
    let running = telchar::persistence::record_backend_running(fixture.url(), "collecting-attempt")
        .expect("running state persists");

    let collecting =
        telchar::persistence::record_backend_completed(fixture.url(), "collecting-attempt")
            .expect("collecting state persists");
    assert_eq!(
        collecting.request.queue_state,
        telchar::persistence::BuildQueueState::Collecting
    );
    assert_eq!(
        collecting.attempt.state,
        telchar::persistence::ExecutionAttemptState::Collecting
    );
    assert!(collecting
        .attempt
        .collecting_at
        .is_some_and(|collecting_at| {
            running
                .attempt
                .started_at
                .is_some_and(|started_at| collecting_at >= started_at)
        }));
    assert!(collecting.attempt.completed_at.is_none());
    assert_eq!(
        collecting.reservation.phase,
        telchar::persistence::CapacityReservationPhase::Collecting
    );
    assert_eq!(collecting.reservation.units, 2);
    assert!(collecting.reservation.released_at.is_none());
    assert!(
        telchar::persistence::read_execution_outcome(fixture.url(), "collecting-attempt")
            .expect("outcome reads")
            .is_none()
    );
    assert_eq!(
        telchar::persistence::record_backend_completed(fixture.url(), "collecting-attempt")
            .expect_err("collection transition cannot repeat")
            .failure(),
        telchar::persistence::ExecutionAttemptFailure::InvalidState
    );

    let mut client = fixture.connect();
    client
        .execute(
            "UPDATE capacity_reservations SET released_at = transaction_timestamp() WHERE reservation_id = 'collecting-reservation'",
            &[],
        )
        .expect("reservation releases for fault fixture");
    client
        .execute(
            "UPDATE build_requests SET queue_state = 'running' WHERE request_id = 'collecting-request'",
            &[],
        )
        .expect("request resets for fault fixture");
    client
        .execute(
            "UPDATE execution_attempts SET state = 'running', collecting_at = NULL WHERE attempt_id = 'collecting-attempt'",
            &[],
        )
        .expect("attempt resets for fault fixture");
    assert_eq!(
        telchar::persistence::record_backend_completed(fixture.url(), "collecting-attempt")
            .expect_err("missing reservation rejects collection transition")
            .failure(),
        telchar::persistence::ExecutionAttemptFailure::InvalidState
    );
    let attempt = telchar::persistence::read_execution_attempt(fixture.url(), "collecting-attempt")
        .expect("attempt reads")
        .expect("attempt exists");
    assert_eq!(
        attempt.state,
        telchar::persistence::ExecutionAttemptState::Running
    );
    assert!(attempt.collecting_at.is_none());
    assert!(attempt.completed_at.is_none());
}

#[test]
fn collecting_attempt_completes_only_with_output_leases_and_result_metadata() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "success-request",
        "/nix/store/11111111111111111111111111111111-success.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "success-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "success-request",
        "/nix/store/11111111111111111111111111111111-success.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "success-request",
        &[(
            "success-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::queue_build_request(fixture.url(), "success-request")
        .expect("request queues");
    telchar::persistence::dispatch_build_request(
        fixture.url(),
        "success-request",
        "success-attempt",
        1,
        "success-request:1",
        "local",
        "success-reservation",
        2,
    )
    .expect("request dispatches");
    telchar::persistence::record_backend_submission(
        fixture.url(),
        "success-attempt",
        "backend-execution-success",
    )
    .expect("submission persists");
    telchar::persistence::record_backend_running(fixture.url(), "success-attempt")
        .expect("running state persists");
    telchar::persistence::record_backend_completed(fixture.url(), "success-attempt")
        .expect("collecting state persists");

    assert_eq!(
        telchar::persistence::complete_execution_success(
            fixture.url(),
            "success-attempt",
            &serde_json::json!({"outputs": 1})
        )
        .expect_err("success requires active output leases")
        .failure(),
        telchar::persistence::ExecutionAttemptFailure::InvalidState
    );
    telchar::persistence::create_request_output_leases(
        fixture.url(),
        "success-request",
        std::time::Duration::from_secs(3_600),
        &[(
            "success-output".to_owned(),
            "/nix/store/33333333333333333333333333333333-output".to_owned(),
        )],
    )
    .expect("output lease persists");
    assert_eq!(
        telchar::persistence::complete_execution_success(
            fixture.url(),
            "success-attempt",
            &serde_json::json!({})
        )
        .expect_err("success requires result metadata")
        .failure(),
        telchar::persistence::ExecutionAttemptFailure::Configuration
    );

    let completed = telchar::persistence::complete_execution_success(
        fixture.url(),
        "success-attempt",
        &serde_json::json!({"outputs": 1}),
    )
    .expect("success persists");
    assert_eq!(
        completed.request.queue_state,
        telchar::persistence::BuildQueueState::Completed
    );
    assert_eq!(
        completed.attempt.state,
        telchar::persistence::ExecutionAttemptState::Succeeded
    );
    assert!(completed.attempt.completed_at.is_some_and(|completed_at| {
        completed
            .attempt
            .collecting_at
            .is_some_and(|collecting_at| completed_at >= collecting_at)
    }));
    assert_eq!(completed.outcome.classification, "succeeded");
    assert_eq!(
        completed.outcome.result_metadata,
        serde_json::json!({"outputs": 1})
    );
    assert!(completed.reservation.released_at.is_some());
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "success-output")
            .expect("output lease reads")
            .expect("output lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
    assert_eq!(
        telchar::persistence::complete_execution_success(
            fixture.url(),
            "success-attempt",
            &serde_json::json!({"outputs": 2})
        )
        .expect_err("terminal success cannot be replaced")
        .failure(),
        telchar::persistence::ExecutionAttemptFailure::InvalidState
    );
}

#[test]
fn queued_request_recovery_is_deterministic_across_restart() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    for (request_id, hash) in [
        ("queued-recovery-b", "22222222222222222222222222222222"),
        ("queued-recovery-a", "11111111111111111111111111111111"),
    ] {
        telchar::persistence::create_build_request(
            fixture.url(),
            request_id,
            &format!("/nix/store/{hash}-{request_id}.drv"),
            "x86_64-linux",
            "test-audit",
            "test-quota",
        )
        .expect("request persists");
        telchar::persistence::create_store_lease(
            fixture.url(),
            &format!("{request_id}-derivation"),
            telchar::persistence::StoreLeaseOwnerKind::Request,
            request_id,
            &format!("/nix/store/{hash}-{request_id}.drv"),
            telchar::persistence::StoreLeasePurpose::Derivation,
        )
        .expect("derivation lease persists");
        telchar::persistence::create_request_input_leases(
            fixture.url(),
            request_id,
            &[(
                format!("{request_id}-input"),
                format!("/nix/store/{hash}-{request_id}-input"),
            )],
        )
        .expect("input lease persists");
        telchar::persistence::queue_build_request(fixture.url(), request_id)
            .expect("request queues");
    }

    let before = telchar::persistence::recover_queued_build_requests(fixture.url(), 256)
        .expect("queued requests recover");
    fixture.restart();
    let after = telchar::persistence::recover_queued_build_requests(fixture.url(), 256)
        .expect("queued requests recover after restart");
    assert_eq!(after, before);
    assert_eq!(
        after
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["queued-recovery-b", "queued-recovery-a"]
    );
    assert!(after
        .iter()
        .all(|request| request.queue_state == telchar::persistence::BuildQueueState::Queued));
    let mut client = fixture.connect();
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM build_requests WHERE queue_state = 'queued'",
                &[]
            )
            .expect("queue count reads")
            .get::<_, i64>(0),
        2
    );
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM execution_attempts", &[])
            .expect("attempt count reads")
            .get::<_, i64>(0),
        0
    );
}

#[test]
fn active_attempt_fails_with_supported_immutable_classification() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "failure-request",
        "/nix/store/11111111111111111111111111111111-failure.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "failure-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "failure-request",
        "/nix/store/11111111111111111111111111111111-failure.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "failure-request",
        &[(
            "failure-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::queue_build_request(fixture.url(), "failure-request")
        .expect("request queues");
    telchar::persistence::dispatch_build_request(
        fixture.url(),
        "failure-request",
        "failure-attempt",
        1,
        "failure-request:1",
        "local",
        "failure-reservation",
        1,
    )
    .expect("request dispatches");

    for classification in ["", "typo-failure"] {
        assert_eq!(
            telchar::persistence::complete_execution_failure(
                fixture.url(),
                "failure-attempt",
                classification,
                &serde_json::json!({"stage": "dispatch"})
            )
            .expect_err("unsupported classification rejects")
            .failure(),
            telchar::persistence::ExecutionAttemptFailure::Configuration
        );
    }
    let failed = telchar::persistence::complete_execution_failure(
        fixture.url(),
        "failure-attempt",
        "infrastructure-failure",
        &serde_json::json!({"stage": "dispatch"}),
    )
    .expect("failure persists");
    assert_eq!(
        failed.request.queue_state,
        telchar::persistence::BuildQueueState::Failed
    );
    assert_eq!(
        failed.attempt.state,
        telchar::persistence::ExecutionAttemptState::Failed
    );
    assert!(failed
        .attempt
        .completed_at
        .is_some_and(|completed_at| completed_at >= failed.attempt.created_at));
    assert_eq!(failed.outcome.classification, "infrastructure-failure");
    assert_eq!(
        failed.outcome.result_metadata,
        serde_json::json!({"stage": "dispatch"})
    );
    assert!(failed.reservation.released_at.is_some());
    assert_eq!(
        telchar::persistence::complete_execution_failure(
            fixture.url(),
            "failure-attempt",
            "build-failure",
            &serde_json::json!({"stage": "build"})
        )
        .expect_err("terminal outcome is immutable")
        .failure(),
        telchar::persistence::ExecutionAttemptFailure::InvalidState
    );

    for classification in [
        "build-failure",
        "admission-failure",
        "input-failure",
        "output-failure",
        "cancelled",
        "internal-failure",
    ] {
        assert_eq!(
            telchar::persistence::complete_execution_failure(
                "",
                "attempt",
                classification,
                &serde_json::json!({}),
            )
            .expect_err("supported classification reaches ordinary input validation")
            .failure(),
            telchar::persistence::ExecutionAttemptFailure::Configuration
        );
    }
}

#[test]
fn dispatch_transaction_rolls_back_when_capacity_reservation_conflicts() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    for (request_id, derivation_hash) in [
        ("reservation-owner", "11111111111111111111111111111111"),
        ("reservation-target", "22222222222222222222222222222222"),
    ] {
        telchar::persistence::create_build_request(
            fixture.url(),
            request_id,
            &format!("/nix/store/{derivation_hash}-{request_id}.drv"),
            "x86_64-linux",
            "test-audit",
            "test-quota",
        )
        .expect("request persists");
    }
    telchar::persistence::create_execution_attempt(
        fixture.url(),
        "reservation-owner-attempt",
        "reservation-owner",
        1,
        "reservation-owner:1",
        "local",
    )
    .expect("reservation owner attempt persists");
    let mut client = fixture.connect();
    client
        .execute(
            "INSERT INTO capacity_reservations (reservation_id, attempt_id, phase, quota_subject, units) VALUES ('shared-reservation', 'reservation-owner-attempt', 'dispatching', 'test-quota', 1)",
            &[],
        )
        .expect("conflicting reservation persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "reservation-target-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "reservation-target",
        "/nix/store/22222222222222222222222222222222-reservation-target.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "reservation-target",
        &[(
            "reservation-target-input".to_owned(),
            "/nix/store/33333333333333333333333333333333-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::queue_build_request(fixture.url(), "reservation-target")
        .expect("request queues");

    assert_eq!(
        telchar::persistence::dispatch_build_request(
            fixture.url(),
            "reservation-target",
            "reservation-target-attempt",
            1,
            "reservation-target:1",
            "local",
            "shared-reservation",
            1,
        )
        .expect_err("reservation conflict rolls back dispatch")
        .failure(),
        telchar::persistence::ExecutionAttemptFailure::Conflict
    );
    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), "reservation-target")
            .expect("request reads")
            .expect("request exists")
            .queue_state,
        telchar::persistence::BuildQueueState::Queued
    );
    assert!(telchar::persistence::read_execution_attempt(
        fixture.url(),
        "reservation-target-attempt"
    )
    .expect("attempt lookup succeeds")
    .is_none());
}

#[test]
fn execution_attempt_persists_stable_identity_and_dispatching_state() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "attempt-request",
        "/nix/store/11111111111111111111111111111111-attempt.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");

    let created = telchar::persistence::create_execution_attempt(
        fixture.url(),
        "attempt-1",
        "attempt-request",
        1,
        "attempt-request:1",
        "local",
    )
    .expect("attempt persists");

    assert_eq!(created.attempt_id, "attempt-1");
    assert_eq!(created.request_id, "attempt-request");
    assert_eq!(created.ordinal, 1);
    assert_eq!(created.idempotency_key, "attempt-request:1");
    assert_eq!(created.backend, "local");
    assert!(created.backend_execution_id.is_none());
    assert_eq!(
        created.state,
        telchar::persistence::ExecutionAttemptState::Dispatching
    );
    assert!(created.submitted_at.is_none());
    assert!(created.started_at.is_none());
    assert!(created.collecting_at.is_none());
    assert!(created.completed_at.is_none());
    assert!(created.fenced_at.is_none());
    fixture.restart();
    assert_eq!(
        telchar::persistence::read_execution_attempt(fixture.url(), "attempt-1")
            .expect("attempt reads"),
        Some(created.clone())
    );
    assert_eq!(
        telchar::persistence::create_execution_attempt(
            fixture.url(),
            "attempt-2",
            "attempt-request",
            1,
            "attempt-request:2",
            "local",
        )
        .expect_err("ordinal is unique per request")
        .failure(),
        telchar::persistence::ExecutionAttemptFailure::Conflict
    );
    assert_eq!(
        telchar::persistence::create_execution_attempt(
            fixture.url(),
            "attempt-2",
            "attempt-request",
            2,
            "attempt-request:1",
            "local",
        )
        .expect_err("idempotency key is globally unique")
        .failure(),
        telchar::persistence::ExecutionAttemptFailure::Conflict
    );
}

#[test]
fn execution_outcome_is_immutable_and_survives_restart() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "outcome-request",
        "/nix/store/11111111111111111111111111111111-outcome.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_execution_attempt(
        fixture.url(),
        "outcome-attempt",
        "outcome-request",
        1,
        "outcome-request:1",
        "local",
    )
    .expect("attempt persists");

    let created = telchar::persistence::create_execution_outcome(
        fixture.url(),
        "outcome-attempt",
        "backend-failure",
    )
    .expect("outcome persists");

    assert_eq!(created.attempt_id, "outcome-attempt");
    assert_eq!(created.classification, "backend-failure");
    fixture.restart();
    assert_eq!(
        telchar::persistence::read_execution_outcome(fixture.url(), "outcome-attempt")
            .expect("outcome reads"),
        Some(created)
    );
    assert_eq!(
        telchar::persistence::create_execution_outcome(
            fixture.url(),
            "outcome-attempt",
            "different-classification",
        )
        .expect_err("terminal outcome cannot be replaced")
        .failure(),
        telchar::persistence::ExecutionOutcomeFailure::Conflict
    );
}

#[test]
fn request_attachment_persists_exact_pair_across_restart() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    let session = telchar::persistence::open_protocol_session(
        fixture.url(),
        "attachment-session",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    let request = telchar::persistence::create_build_request(
        fixture.url(),
        "attachment-request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");

    let attached = telchar::persistence::attach_request(
        fixture.url(),
        &session.session_id,
        &request.request_id,
    )
    .expect("request attaches");
    assert_eq!(attached.session_id, session.session_id);
    assert_eq!(attached.request_id, request.request_id);
    assert_eq!(
        attached.state,
        telchar::persistence::RequestAttachmentState::Attached
    );
    assert!(attached.detached_at.is_none());
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            &session.session_id,
            &request.request_id,
        )
        .expect("attachment reads"),
        Some(attached.clone())
    );
    assert!(telchar::persistence::read_request_attachment(
        fixture.url(),
        &session.session_id,
        "absent-request",
    )
    .expect("absent attachment reads")
    .is_none());

    fixture.restart();

    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            &session.session_id,
            &request.request_id,
        )
        .expect("attachment reads after restart"),
        Some(attached)
    );
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.url(), &session.session_id)
            .expect("session reads after restart"),
        Some(session)
    );
    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), &request.request_id)
            .expect("request reads after restart"),
        Some(request)
    );
}

#[test]
fn request_attachment_rejects_invalid_references_and_duplicate_without_mutation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "open-session",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "closed-session",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::close_protocol_session(fixture.url(), "closed-session")
        .expect("session closes");
    telchar::persistence::create_build_request(
        fixture.url(),
        "request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");

    for (database_url, session_id, request_id) in [
        (fixture.url(), "", "request"),
        (
            fixture.url(),
            &"x".repeat(telchar::ipc::MAX_IPC_COMPONENT_BYTES + 1),
            "request",
        ),
        (fixture.url(), "open-session", ""),
        (
            fixture.url(),
            "open-session",
            &"x".repeat(telchar::ipc::MAX_IPC_COMPONENT_BYTES + 1),
        ),
        ("", "open-session", "request"),
    ] {
        let error = telchar::persistence::attach_request(database_url, session_id, request_id)
            .expect_err("invalid attachment rejects");
        assert_eq!(
            error.failure(),
            telchar::persistence::RequestAttachmentFailure::Configuration
        );
        assert_eq!(
            error.to_string(),
            "request attachment state operation failed"
        );
    }
    assert_eq!(
        telchar::persistence::attach_request(fixture.url(), "missing", "request")
            .expect_err("missing session rejects")
            .failure(),
        telchar::persistence::RequestAttachmentFailure::Missing
    );
    assert_eq!(
        telchar::persistence::attach_request(fixture.url(), "open-session", "missing")
            .expect_err("missing request rejects")
            .failure(),
        telchar::persistence::RequestAttachmentFailure::Missing
    );
    assert_eq!(
        telchar::persistence::attach_request(fixture.url(), "closed-session", "request")
            .expect_err("closed session rejects")
            .failure(),
        telchar::persistence::RequestAttachmentFailure::InvalidState
    );

    let attached = telchar::persistence::attach_request(fixture.url(), "open-session", "request")
        .expect("attachment persists");
    assert_eq!(
        telchar::persistence::attach_request(fixture.url(), "open-session", "request")
            .expect_err("duplicate attachment rejects")
            .failure(),
        telchar::persistence::RequestAttachmentFailure::Conflict
    );
    assert_eq!(
        telchar::persistence::read_request_attachment(fixture.url(), "open-session", "request")
            .expect("attachment reads"),
        Some(attached)
    );
}

#[test]
fn request_attachment_completed_delivery_survives_restart() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "delivery-session",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "delivery-request",
        "/nix/store/11111111111111111111111111111111-delivery.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let attached =
        telchar::persistence::attach_request(fixture.url(), "delivery-session", "delivery-request")
            .expect("request attaches");

    let delivered = telchar::persistence::complete_request_delivery(
        fixture.url(),
        "delivery-session",
        "delivery-request",
    )
    .expect("delivery completes");

    assert_eq!(delivered.session_id, attached.session_id);
    assert_eq!(delivered.request_id, attached.request_id);
    assert_eq!(delivered.attached_at, attached.attached_at);
    assert_eq!(
        delivered.state,
        telchar::persistence::RequestAttachmentState::Delivered
    );
    assert!(delivered.detached_at.is_none());
    assert!(delivered
        .delivered_at
        .is_some_and(|delivered_at| delivered_at >= attached.attached_at));
    fixture.restart();
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "delivery-session",
            "delivery-request",
        )
        .expect("delivered attachment reads"),
        Some(delivered)
    );
    assert_eq!(
        telchar::persistence::complete_request_delivery(
            fixture.url(),
            "delivery-session",
            "delivery-request",
        )
        .expect_err("delivery cannot complete twice")
        .failure(),
        telchar::persistence::RequestAttachmentFailure::InvalidState
    );
    assert_eq!(
        telchar::persistence::detach_request(
            fixture.url(),
            "delivery-session",
            "delivery-request",
        )
        .expect_err("delivered attachment cannot detach")
        .failure(),
        telchar::persistence::RequestAttachmentFailure::InvalidState
    );
}

#[test]
fn request_attachment_detaches_once_without_mutating_references() {
    let fixture = Arc::new(PostgresFixture::start());
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    let session = telchar::persistence::open_protocol_session(
        fixture.url(),
        "detach-session",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    let request = telchar::persistence::create_build_request(
        fixture.url(),
        "detach-request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let attached = telchar::persistence::attach_request(
        fixture.url(),
        &session.session_id,
        &request.request_id,
    )
    .expect("attachment persists");

    let first_url = fixture.url().to_owned();
    let second_url = fixture.url().to_owned();
    let first = thread::spawn(move || {
        telchar::persistence::detach_request(&first_url, "detach-session", "detach-request")
    });
    let second = thread::spawn(move || {
        telchar::persistence::detach_request(&second_url, "detach-session", "detach-request")
    });
    let outcomes = [
        first.join().expect("first detacher does not panic"),
        second.join().expect("second detacher does not panic"),
    ];

    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .map(telchar::persistence::RequestAttachmentError::failure)
            .collect::<Vec<_>>(),
        vec![telchar::persistence::RequestAttachmentFailure::InvalidState]
    );
    let detached = outcomes
        .into_iter()
        .find_map(Result::ok)
        .expect("one detachment succeeds");
    assert_eq!(
        detached.state,
        telchar::persistence::RequestAttachmentState::Detached
    );
    assert_eq!(detached.attached_at, attached.attached_at);
    assert!(detached
        .detached_at
        .is_some_and(|detached_at| detached_at >= attached.attached_at));
    assert_eq!(
        telchar::persistence::detach_request(fixture.url(), "missing", "request")
            .expect_err("missing attachment rejects")
            .failure(),
        telchar::persistence::RequestAttachmentFailure::Missing
    );
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.url(), &session.session_id)
            .expect("session reads"),
        Some(session)
    );
    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), &request.request_id)
            .expect("request reads"),
        Some(request)
    );
}

#[test]
fn malformed_request_attachment_rows_fail_closed() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "malformed-session",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "malformed-request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::attach_request(fixture.url(), "malformed-session", "malformed-request")
        .expect("attachment persists");
    fixture
        .connect()
        .batch_execute(
            "ALTER TABLE request_attachments DROP CONSTRAINT request_attachments_state_check; ALTER TABLE request_attachments DROP CONSTRAINT request_attachments_terminal_at_check; UPDATE request_attachments SET state = 'malformed' WHERE session_id = 'malformed-session' AND request_id = 'malformed-request'",
        )
        .expect("malformed attachment row writes");

    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "malformed-session",
            "malformed-request",
        )
        .expect_err("malformed attachment rejects")
        .failure(),
        telchar::persistence::RequestAttachmentFailure::Query
    );
    assert_eq!(
        telchar::persistence::detach_request(
            fixture.url(),
            "malformed-session",
            "malformed-request",
        )
        .expect_err("malformed attachment cannot detach")
        .failure(),
        telchar::persistence::RequestAttachmentFailure::Query
    );
}

#[test]
fn attach_rejects_malformed_referenced_session() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "invalid-reference-session",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "invalid-reference-request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    fixture
        .connect()
        .execute(
            "UPDATE protocol_sessions SET requester_reference = 'malformed' WHERE session_id = 'invalid-reference-session'",
            &[],
        )
        .expect("malformed session writes");

    assert_eq!(
        telchar::persistence::attach_request(
            fixture.url(),
            "invalid-reference-session",
            "invalid-reference-request",
        )
        .expect_err("malformed session rejects")
        .failure(),
        telchar::persistence::RequestAttachmentFailure::Query
    );
    assert!(telchar::persistence::read_request_attachment(
        fixture.url(),
        "invalid-reference-session",
        "invalid-reference-request",
    )
    .expect("attachment reads")
    .is_none());
}

#[test]
fn failed_request_attachment_statements_and_commits_do_not_persist_transitions() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "failure-session",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "failure-request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let mut client = fixture.connect();
    client
        .batch_execute(
            "CREATE FUNCTION reject_attachment_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject insert'; END $$; CREATE TRIGGER reject_attachment_insert BEFORE INSERT ON request_attachments FOR EACH ROW EXECUTE FUNCTION reject_attachment_insert();",
        )
        .expect("insert failure trigger installs");
    assert_eq!(
        telchar::persistence::attach_request(fixture.url(), "failure-session", "failure-request")
            .expect_err("attachment statement rejects")
            .failure(),
        telchar::persistence::RequestAttachmentFailure::Query
    );
    assert!(telchar::persistence::read_request_attachment(
        fixture.url(),
        "failure-session",
        "failure-request",
    )
    .expect("attachment reads")
    .is_none());
    client
        .batch_execute("DROP TRIGGER reject_attachment_insert ON request_attachments; DROP FUNCTION reject_attachment_insert()")
        .expect("insert failure trigger removes");
    let attached =
        telchar::persistence::attach_request(fixture.url(), "failure-session", "failure-request")
            .expect("attachment persists");
    client
        .batch_execute(
            "CREATE FUNCTION reject_attachment_detach() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject detach'; END $$; CREATE TRIGGER reject_attachment_detach BEFORE UPDATE ON request_attachments FOR EACH ROW EXECUTE FUNCTION reject_attachment_detach();",
        )
        .expect("detach failure trigger installs");
    assert_eq!(
        telchar::persistence::detach_request(fixture.url(), "failure-session", "failure-request")
            .expect_err("detach statement rejects")
            .failure(),
        telchar::persistence::RequestAttachmentFailure::Query
    );
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "failure-session",
            "failure-request",
        )
        .expect("attachment reads"),
        Some(attached.clone())
    );
    client
        .batch_execute("DROP TRIGGER reject_attachment_detach ON request_attachments; DROP FUNCTION reject_attachment_detach()")
        .expect("detach failure trigger removes");
    client
        .batch_execute(
            "CREATE FUNCTION reject_attachment_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject commit'; END $$; CREATE CONSTRAINT TRIGGER reject_attachment_commit AFTER UPDATE ON request_attachments DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_attachment_commit();",
        )
        .expect("commit failure trigger installs");
    assert_eq!(
        telchar::persistence::detach_request(fixture.url(), "failure-session", "failure-request")
            .expect_err("detach commit rejects")
            .failure(),
        telchar::persistence::RequestAttachmentFailure::Commit
    );
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "failure-session",
            "failure-request",
        )
        .expect("attachment reads"),
        Some(attached)
    );
}

#[test]
fn build_request_state_rejects_invalid_inputs_and_conflicts_without_mutation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let path = "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv";
    let maximum_path = format!(
        "/{}",
        "x".repeat(nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES - 1)
    );
    telchar::persistence::create_build_request(
        fixture.url(),
        "maximum-path",
        &maximum_path,
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("maximum protocol store path persists");

    for (database_url, request_id, derivation_path, system) in [
        (fixture.url(), "", path, "x86_64-linux"),
        (
            fixture.url(),
            &"x".repeat(telchar::ipc::MAX_IPC_COMPONENT_BYTES + 1),
            path,
            "x86_64-linux",
        ),
        (fixture.url(), "invalid-path", "", "x86_64-linux"),
        (
            fixture.url(),
            "oversized-path",
            &"x".repeat(nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES + 1),
            "x86_64-linux",
        ),
        (fixture.url(), "invalid-system", path, ""),
        (
            fixture.url(),
            "oversized-system",
            path,
            &"x".repeat(telchar::ipc::MAX_IPC_COMPONENT_BYTES + 1),
        ),
        ("", "missing-url", path, "x86_64-linux"),
    ] {
        let error = telchar::persistence::create_build_request(
            database_url,
            request_id,
            derivation_path,
            system,
            "test-audit",
            "test-quota",
        )
        .expect_err("invalid build request rejects");
        assert_eq!(
            error.failure(),
            telchar::persistence::BuildRequestFailure::Configuration
        );
        assert_eq!(error.to_string(), "build request state operation failed");
    }

    let first = telchar::persistence::create_build_request(
        fixture.url(),
        "request-1",
        path,
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("first request persists");
    for (derivation_path, system) in [(path, "x86_64-linux"), ("/nix/store/other.drv", "other")] {
        assert_eq!(
            telchar::persistence::create_build_request(
                fixture.url(),
                "request-1",
                derivation_path,
                system,
                "test-audit",
                "test-quota",
            )
            .expect_err("duplicate request rejects")
            .failure(),
            telchar::persistence::BuildRequestFailure::Conflict
        );
    }
    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), "request-1")
            .expect("request reads"),
        Some(first)
    );
}

#[test]
fn build_request_state_survives_restart_and_malformed_rows_fail_closed() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let created = telchar::persistence::create_build_request(
        fixture.url(),
        "restart-request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");

    fixture.restart();

    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), "restart-request")
            .expect("request reads"),
        Some(created)
    );
    fixture
        .connect()
        .execute(
            "INSERT INTO build_requests (request_id, derivation_path, system) VALUES ('malformed', $1, 'x86_64-linux')",
            &[&"x".repeat(nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES + 1)],
        )
        .expect("malformed row inserts");
    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), "malformed")
            .expect_err("malformed request rejects")
            .failure(),
        telchar::persistence::BuildRequestFailure::Query
    );
}

#[test]
fn failed_build_request_statement_and_commit_do_not_persist_rows() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let mut client = fixture.connect();
    client
        .batch_execute(
            "CREATE FUNCTION reject_build_request_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject insert'; END $$; CREATE TRIGGER reject_build_request_insert BEFORE INSERT ON build_requests FOR EACH ROW EXECUTE FUNCTION reject_build_request_insert();",
        )
        .expect("statement failure trigger installs");

    let error = telchar::persistence::create_build_request(
        fixture.url(),
        "failed-statement",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect_err("statement failure rejects");
    assert_eq!(
        error.failure(),
        telchar::persistence::BuildRequestFailure::Query
    );
    assert!(
        telchar::persistence::read_build_request(fixture.url(), "failed-statement")
            .expect("request reads")
            .is_none()
    );
    client
        .batch_execute("DROP TRIGGER reject_build_request_insert ON build_requests; DROP FUNCTION reject_build_request_insert()")
        .expect("statement failure trigger removes");
    client
        .batch_execute(
            "CREATE FUNCTION reject_build_request_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject commit'; END $$; CREATE CONSTRAINT TRIGGER reject_build_request_commit AFTER INSERT ON build_requests DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_build_request_commit();",
        )
        .expect("commit failure trigger installs");

    let error = telchar::persistence::create_build_request(
        fixture.url(),
        "failed-commit",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect_err("commit failure rejects");
    assert_eq!(
        error.failure(),
        telchar::persistence::BuildRequestFailure::Commit
    );
    assert!(
        telchar::persistence::read_build_request(fixture.url(), "failed-commit")
            .expect("request reads")
            .is_none()
    );
}

#[test]
fn duplicate_and_invalid_protocol_session_opens_reject_without_mutation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "session-1",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("first open succeeds");

    for reference in [
        requester_reference,
        "d97fcc940167deddfb3b76c3a5398037de37729288d7111cad50e693000e2ec3",
    ] {
        let error = telchar::persistence::open_protocol_session(
            fixture.url(),
            "session-1",
            reference,
            "test-audit",
            "test-quota",
        )
        .expect_err("duplicate session rejects");
        assert_eq!(
            error.failure(),
            telchar::persistence::ProtocolSessionFailure::Conflict
        );
        assert_eq!(error.to_string(), "protocol session state operation failed");
    }
    for (session_id, reference) in [
        ("", requester_reference),
        (
            &"x".repeat(telchar::ipc::MAX_IPC_COMPONENT_BYTES + 1),
            requester_reference,
        ),
        ("other", "not-hex"),
        (
            "other",
            "F3D3E3C63821A33F175CBE0DC4288E6E906EC8FE000DF17C91D6AE616CC4AB1E",
        ),
    ] {
        let error = telchar::persistence::open_protocol_session(
            fixture.url(),
            session_id,
            reference,
            "test-audit",
            "test-quota",
        )
        .expect_err("invalid open rejects");
        assert_eq!(
            error.failure(),
            telchar::persistence::ProtocolSessionFailure::Configuration
        );
    }

    let read = telchar::persistence::read_protocol_session(fixture.url(), "session-1")
        .expect("session reads")
        .expect("session remains");
    assert_eq!(read.requester_reference, requester_reference);
    assert!(
        telchar::persistence::read_protocol_session(fixture.url(), "other")
            .expect("other reads")
            .is_none()
    );
}

#[test]
fn close_protocol_session_persists_exactly_once() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    let opened = telchar::persistence::open_protocol_session(
        fixture.url(),
        "session-1",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");

    let closed = telchar::persistence::close_protocol_session(fixture.url(), "session-1")
        .expect("session closes");

    assert_eq!(closed.session_id, opened.session_id);
    assert_eq!(closed.requester_reference, requester_reference);
    assert_eq!(
        closed.state,
        telchar::persistence::ProtocolSessionState::Closed
    );
    assert!(closed
        .closed_at
        .is_some_and(|closed_at| closed_at >= opened.created_at));
    assert_eq!(
        telchar::persistence::close_protocol_session(fixture.url(), "session-1")
            .expect_err("closed session does not close again")
            .failure(),
        telchar::persistence::ProtocolSessionFailure::InvalidTransition
    );
    assert_eq!(
        telchar::persistence::close_protocol_session(fixture.url(), "missing")
            .expect_err("missing session rejects")
            .failure(),
        telchar::persistence::ProtocolSessionFailure::NotFound
    );
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.url(), "session-1")
            .expect("session reads"),
        Some(closed)
    );
}

#[test]
fn concurrent_protocol_session_closers_have_one_winner() {
    let fixture = Arc::new(PostgresFixture::start());
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "session-1",
        "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    let first_url = fixture.url().to_owned();
    let second_url = fixture.url().to_owned();
    let first = thread::spawn(move || {
        telchar::persistence::close_protocol_session(&first_url, "session-1")
    });
    let second = thread::spawn(move || {
        telchar::persistence::close_protocol_session(&second_url, "session-1")
    });

    let outcomes = [
        first.join().expect("first closer does not panic"),
        second.join().expect("second closer does not panic"),
    ];

    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .map(telchar::persistence::ProtocolSessionError::failure)
            .collect::<Vec<_>>(),
        vec![telchar::persistence::ProtocolSessionFailure::InvalidTransition]
    );
}

#[test]
fn protocol_session_state_survives_database_restart() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    let opened = telchar::persistence::open_protocol_session(
        fixture.url(),
        "open-session",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("open session persists");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "closed-session",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("closed session opens");
    let closed = telchar::persistence::close_protocol_session(fixture.url(), "closed-session")
        .expect("closed session persists");

    fixture.restart();

    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.url(), "open-session")
            .expect("open session reads"),
        Some(opened)
    );
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.url(), "closed-session")
            .expect("closed session reads"),
        Some(closed)
    );
}

#[test]
fn malformed_persisted_protocol_session_decodes_as_query_failure() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    fixture
        .connect()
        .execute(
            "INSERT INTO protocol_sessions (session_id, requester_reference, state) VALUES ('malformed', 'not-a-reference', 'open')",
            &[],
        )
        .expect("malformed session inserts");

    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.url(), "malformed")
            .expect_err("malformed session rejects")
            .failure(),
        telchar::persistence::ProtocolSessionFailure::Query
    );
}

#[test]
fn failed_protocol_session_statements_do_not_claim_or_persist_transitions() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    let mut client = fixture.connect();
    client
        .batch_execute(
            "CREATE FUNCTION reject_protocol_session_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject insert'; END $$; CREATE TRIGGER reject_protocol_session_insert BEFORE INSERT ON protocol_sessions FOR EACH ROW EXECUTE FUNCTION reject_protocol_session_insert();",
        )
        .expect("failure trigger installs");

    let error = telchar::persistence::open_protocol_session(
        fixture.url(),
        "failed-open",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect_err("statement failure rejects open");
    assert_eq!(
        error.failure(),
        telchar::persistence::ProtocolSessionFailure::Query
    );
    assert_eq!(error.to_string(), "protocol session state operation failed");
    assert!(
        telchar::persistence::read_protocol_session(fixture.url(), "failed-open")
            .expect("failed row reads")
            .is_none()
    );
    client
        .batch_execute("DROP TRIGGER reject_protocol_session_insert ON protocol_sessions; DROP FUNCTION reject_protocol_session_insert()")
        .expect("failure trigger removes");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "failed-close",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    client
        .batch_execute(
            "CREATE FUNCTION reject_protocol_session_close() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject close'; END $$; CREATE TRIGGER reject_protocol_session_close BEFORE UPDATE ON protocol_sessions FOR EACH ROW EXECUTE FUNCTION reject_protocol_session_close();",
        )
        .expect("failure trigger installs");

    let error = telchar::persistence::close_protocol_session(fixture.url(), "failed-close")
        .expect_err("statement failure rejects close");
    assert_eq!(
        error.failure(),
        telchar::persistence::ProtocolSessionFailure::Query
    );
    assert_eq!(error.to_string(), "protocol session state operation failed");
    assert_eq!(
        telchar::persistence::read_protocol_session(fixture.url(), "failed-close")
            .expect("open row reads")
            .expect("open row remains")
            .state,
        telchar::persistence::ProtocolSessionState::Open
    );
}

#[test]
fn store_lease_persists_across_restart() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "lease-request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");

    let created = telchar::persistence::create_store_lease(
        fixture.url(),
        "lease-1",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "lease-request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");

    fixture.restart();

    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "lease-1").expect("lease reads"),
        Some(created)
    );
}

#[test]
fn store_lease_releases_once_without_mutating_immutable_fields() {
    let fixture = Arc::new(PostgresFixture::start());
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "release-request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let created = telchar::persistence::create_store_lease(
        fixture.url(),
        "release-lease",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "release-request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        telchar::persistence::StoreLeasePurpose::Output,
    )
    .expect("lease persists");

    let first_url = fixture.url().to_owned();
    let second_url = fixture.url().to_owned();
    let first = thread::spawn(move || {
        telchar::persistence::release_store_lease(&first_url, "release-lease")
    });
    let second = thread::spawn(move || {
        telchar::persistence::release_store_lease(&second_url, "release-lease")
    });
    let outcomes = [
        first.join().expect("first releaser does not panic"),
        second.join().expect("second releaser does not panic"),
    ];

    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .map(telchar::persistence::StoreLeaseError::failure)
            .collect::<Vec<_>>(),
        vec![telchar::persistence::StoreLeaseFailure::InvalidState]
    );
    let released = outcomes
        .into_iter()
        .find_map(Result::ok)
        .expect("one release succeeds");
    assert_eq!(released.lease_id, created.lease_id);
    assert_eq!(released.owner_kind, created.owner_kind);
    assert_eq!(released.owner_id, created.owner_id);
    assert_eq!(released.store_path, created.store_path);
    assert_eq!(released.purpose, created.purpose);
    assert_eq!(released.created_at, created.created_at);
    assert_eq!(
        released.state,
        telchar::persistence::StoreLeaseState::Released
    );
    assert!(released
        .released_at
        .is_some_and(|at| at >= created.created_at));
    assert_eq!(
        telchar::persistence::release_store_lease(fixture.url(), "release-lease")
            .expect_err("released lease does not release again")
            .failure(),
        telchar::persistence::StoreLeaseFailure::InvalidState
    );
    assert_eq!(
        telchar::persistence::release_store_lease(fixture.url(), "missing")
            .expect_err("absent lease rejects")
            .failure(),
        telchar::persistence::StoreLeaseFailure::Missing
    );
}

#[test]
fn duplicate_store_lease_id_rejects_without_mutating_original() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "duplicate-owner",
        "/nix/store/11111111111111111111111111111111-original.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let original = telchar::persistence::create_store_lease(
        fixture.url(),
        "duplicate-lease",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "duplicate-owner",
        "/nix/store/11111111111111111111111111111111-original.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");

    assert_eq!(
        telchar::persistence::create_store_lease(
            fixture.url(),
            "duplicate-lease",
            telchar::persistence::StoreLeaseOwnerKind::Request,
            "duplicate-owner",
            "/nix/store/22222222222222222222222222222222-different",
            telchar::persistence::StoreLeasePurpose::Output,
        )
        .expect_err("duplicate lease rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::Conflict
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "duplicate-lease")
            .expect("lease reads"),
        Some(original)
    );
}

#[test]
fn store_lease_rejects_statement_and_commit_failures_without_transition() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "failure-owner",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let mut client = fixture.connect();
    client.batch_execute(
        "CREATE FUNCTION reject_store_lease_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject insert'; END $$;
         CREATE TRIGGER reject_store_lease_insert BEFORE INSERT ON store_leases FOR EACH ROW EXECUTE FUNCTION reject_store_lease_insert();",
    ).expect("insert trigger installs");
    assert_eq!(
        telchar::persistence::create_store_lease(
            fixture.url(),
            "statement-failure",
            telchar::persistence::StoreLeaseOwnerKind::Request,
            "failure-owner",
            "/nix/store/11111111111111111111111111111111-path",
            telchar::persistence::StoreLeasePurpose::Input,
        )
        .expect_err("insert rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::Query
    );
    assert!(
        telchar::persistence::read_store_lease(fixture.url(), "statement-failure")
            .expect("absent lease reads")
            .is_none()
    );
    client
        .batch_execute(
            "DROP TRIGGER reject_store_lease_insert ON store_leases;
         DROP FUNCTION reject_store_lease_insert();",
        )
        .expect("insert trigger removes");
    let lease = telchar::persistence::create_store_lease(
        fixture.url(),
        "commit-failure",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "failure-owner",
        "/nix/store/11111111111111111111111111111111-path",
        telchar::persistence::StoreLeasePurpose::Input,
    )
    .expect("lease persists");
    client.batch_execute(
        "CREATE FUNCTION reject_store_lease_release() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject release'; END $$;
         CREATE TRIGGER reject_store_lease_release BEFORE UPDATE ON store_leases FOR EACH ROW EXECUTE FUNCTION reject_store_lease_release();",
    ).expect("release trigger installs");
    assert_eq!(
        telchar::persistence::release_store_lease(fixture.url(), "commit-failure")
            .expect_err("release statement rejects")
            .failure(),
        telchar::persistence::StoreLeaseFailure::Query
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "commit-failure")
            .expect("lease reads"),
        Some(lease.clone())
    );
    client.batch_execute(
        "DROP TRIGGER reject_store_lease_release ON store_leases;
         DROP FUNCTION reject_store_lease_release();
         CREATE FUNCTION reject_store_lease_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject commit'; END $$;
         CREATE CONSTRAINT TRIGGER reject_store_lease_commit AFTER UPDATE ON store_leases DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_store_lease_commit();",
    ).expect("commit trigger installs");
    assert_eq!(
        telchar::persistence::release_store_lease(fixture.url(), "commit-failure")
            .expect_err("release commit rejects")
            .failure(),
        telchar::persistence::StoreLeaseFailure::Commit
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "commit-failure")
            .expect("lease reads"),
        Some(lease)
    );
}

#[test]
fn store_lease_telemetry_and_errors_are_bounded_and_redacted() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "telemetry-owner",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let captured = EventCapture::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(captured.clone()));
    tracing::dispatcher::with_default(&dispatch, || {
        telchar::persistence::create_store_lease(
            fixture.url(),
            "telemetry-lease",
            telchar::persistence::StoreLeaseOwnerKind::Request,
            "telemetry-owner",
            "/nix/store/11111111111111111111111111111111-telemetry",
            telchar::persistence::StoreLeasePurpose::Output,
        )
        .expect("lease persists");
        telchar::persistence::release_store_lease(fixture.url(), "telemetry-lease")
            .expect("lease releases");
        let error = telchar::persistence::create_store_lease(
            "postgresql://sensitive-url",
            "sensitive-lease",
            telchar::persistence::StoreLeaseOwnerKind::Request,
            "sensitive-owner",
            "relative",
            telchar::persistence::StoreLeasePurpose::Input,
        )
        .expect_err("invalid lease rejects");
        assert_eq!(error.to_string(), "store lease state operation failed");
        assert_eq!(
            error.failure(),
            telchar::persistence::StoreLeaseFailure::Configuration
        );
    });
    let events = captured.events();
    assert!(events
        .iter()
        .any(|event| event.contains("database.store_lease.created")
            && event.contains("operation=\"create\"")));
    assert!(events
        .iter()
        .any(|event| event.contains("database.store_lease.released")
            && event.contains("operation=\"release\"")));
    assert!(events
        .iter()
        .any(|event| event.contains("database.store_lease.failed")
            && event.contains("operation=\"create\"")
            && event.contains("failure_class=\"configuration\"")));
    for forbidden in [
        fixture.url(),
        "sensitive-url",
        "sensitive-lease",
        "sensitive-owner",
        "/nix/store/11111111111111111111111111111111-telemetry",
    ] {
        assert!(
            !events.iter().any(|event| event.contains(forbidden)),
            "{events:?}"
        );
    }
}

#[test]
fn store_lease_validates_owners_and_rejects_malformed_rows() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "open-owner",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "closed-owner",
        requester_reference,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::close_protocol_session(fixture.url(), "closed-owner")
        .expect("session closes");

    for (owner_kind, purpose) in [
        (
            telchar::persistence::StoreLeaseOwnerKind::Session,
            telchar::persistence::StoreLeasePurpose::Input,
        ),
        (
            telchar::persistence::StoreLeaseOwnerKind::Session,
            telchar::persistence::StoreLeasePurpose::Transfer,
        ),
    ] {
        let record = telchar::persistence::create_store_lease(
            fixture.url(),
            purpose_name(purpose),
            owner_kind,
            "open-owner",
            "/nix/store/11111111111111111111111111111111-input",
            purpose,
        )
        .expect("session lease persists");
        assert_eq!(record.owner_kind, owner_kind);
        assert_eq!(record.purpose, purpose);
    }
    assert_eq!(
        telchar::persistence::create_store_lease(
            fixture.url(),
            "missing-owner",
            telchar::persistence::StoreLeaseOwnerKind::Request,
            "missing",
            "/nix/store/11111111111111111111111111111111-path",
            telchar::persistence::StoreLeasePurpose::Input,
        )
        .expect_err("missing owner rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::Missing
    );
    assert_eq!(
        telchar::persistence::create_store_lease(
            fixture.url(),
            "closed-owner",
            telchar::persistence::StoreLeaseOwnerKind::Session,
            "closed-owner",
            "/nix/store/11111111111111111111111111111111-path",
            telchar::persistence::StoreLeasePurpose::Transfer,
        )
        .expect_err("closed owner rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::InvalidState
    );
    for (lease_id, store_path) in [
        ("", "/nix/store/path"),
        ("invalid-path", "relative"),
        ("newline-path", "/nix/store/path\n"),
        ("nul-path", "/nix/store/path\0"),
    ] {
        assert_eq!(
            telchar::persistence::create_store_lease(
                "postgresql://127.0.0.1:1/no_connection",
                lease_id,
                telchar::persistence::StoreLeaseOwnerKind::Session,
                "open-owner",
                store_path,
                telchar::persistence::StoreLeasePurpose::Input,
            )
            .expect_err("invalid input rejects before connection")
            .failure(),
            telchar::persistence::StoreLeaseFailure::Configuration
        );
    }

    let mut client = fixture.connect();
    client
        .batch_execute(
            "ALTER TABLE store_leases DROP CONSTRAINT store_leases_owner_kind_check;
         ALTER TABLE store_leases DROP CONSTRAINT store_leases_purpose_check;
         ALTER TABLE store_leases DROP CONSTRAINT store_leases_state_check;
         ALTER TABLE store_leases DROP CONSTRAINT store_leases_store_path_check;
         ALTER TABLE store_leases DROP CONSTRAINT store_leases_released_at_check;
         INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state)
         VALUES ('malformed-lease', 'unknown', 'owner', 'relative', 'unknown', 'active'),
                ('malformed-active', 'request', 'owner', '/nix/store/path', 'input', 'active'),
                ('malformed-released', 'request', 'owner', '/nix/store/path', 'input', 'released');
         UPDATE store_leases SET released_at = transaction_timestamp() WHERE lease_id = 'malformed-active';
         UPDATE store_leases SET released_at = NULL WHERE lease_id = 'malformed-released';",
        )
        .expect("malformed lease writes");
    for lease_id in ["malformed-lease", "malformed-active", "malformed-released"] {
        assert_eq!(
            telchar::persistence::read_store_lease(fixture.url(), lease_id)
                .expect_err("malformed lease rejects")
                .failure(),
            telchar::persistence::StoreLeaseFailure::Query
        );
    }
    assert_eq!(
        telchar::persistence::release_store_lease(fixture.url(), "malformed-active")
            .expect_err("malformed active lease rejects")
            .failure(),
        telchar::persistence::StoreLeaseFailure::Query
    );
}

#[derive(Clone, Default)]
struct EventCapture(Arc<Mutex<Vec<String>>>);

impl EventCapture {
    fn events(&self) -> Vec<String> {
        self.0.lock().expect("events lock").clone()
    }
}

impl<S> Layer<S> for EventCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        self.0.lock().expect("events lock").push(fields.0);
    }
}

#[derive(Default)]
struct EventFields(String);

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push_str(field.name());
        self.0.push('=');
        self.0.push_str(&format!("{value:?}"));
    }
}

fn purpose_name(purpose: telchar::persistence::StoreLeasePurpose) -> &'static str {
    match purpose {
        telchar::persistence::StoreLeasePurpose::Input => "session-input",
        telchar::persistence::StoreLeasePurpose::Transfer => "session-transfer",
        _ => unreachable!("tested purpose has a stable fixture name"),
    }
}

#[test]
fn empty_database_migrates_to_minimum_lifecycle_schema() {
    let fixture = PostgresFixture::start();

    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");

    let mut client = fixture.connect();
    let ledger = client
        .query(
            "SELECT version, name, checksum FROM telchar_schema_migrations ORDER BY version",
            &[],
        )
        .expect("migration ledger reads");
    assert_eq!(ledger.len(), 5);
    assert_eq!(ledger[0].get::<_, i64>(0), 1);
    assert_eq!(ledger[0].get::<_, String>(1), "minimum_lifecycle");
    assert_eq!(ledger[0].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[1].get::<_, i64>(0), 2);
    assert_eq!(ledger[1].get::<_, String>(1), "output_retention");
    assert_eq!(ledger[1].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[2].get::<_, i64>(0), 3);
    assert_eq!(ledger[2].get::<_, String>(1), "execution_state");
    assert_eq!(ledger[2].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[3].get::<_, i64>(0), 4);
    assert_eq!(ledger[3].get::<_, String>(1), "reconciliation_state");
    assert_eq!(ledger[3].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[4].get::<_, i64>(0), 5);
    assert_eq!(ledger[4].get::<_, String>(1), "local_backend_registry");
    assert_eq!(ledger[4].get::<_, Vec<u8>>(2).len(), 32);

    for table in [
        "protocol_sessions",
        "build_requests",
        "request_attachments",
        "store_leases",
        "execution_attempts",
        "execution_outcomes",
        "capacity_reservations",
        "local_backend_executions",
    ] {
        let row = client
            .query_one("SELECT to_regclass($1)::text", &[&table])
            .expect("table lookup succeeds");
        assert_eq!(row.get::<_, Option<String>>(0).as_deref(), Some(table));
    }

    assert_eq!(
        client
            .query_one(
                "SELECT data_type FROM information_schema.columns WHERE table_name = 'telchar_schema_migrations' AND column_name = 'checksum'",
                &[],
            )
            .expect("checksum type reads")
            .get::<_, String>(0),
        Type::BYTEA.name()
    );
}

#[test]
fn reconciliation_state_migration_preserves_existing_execution_rows() {
    let fixture = PostgresFixture::start();
    let migrations = [
        (
            1_i64,
            "minimum_lifecycle",
            include_str!("../migrations/0001_minimum_lifecycle.sql"),
        ),
        (
            2_i64,
            "output_retention",
            include_str!("../migrations/0002_output_retention.sql"),
        ),
        (
            3_i64,
            "execution_state",
            include_str!("../migrations/0003_execution_state.sql"),
        ),
    ];
    let mut client = fixture.connect();
    for (_, _, sql) in migrations {
        client.batch_execute(sql).expect("prior migration applies");
    }
    client
        .batch_execute(
            "CREATE TABLE telchar_schema_migrations (
                 version bigint PRIMARY KEY,
                 name text NOT NULL UNIQUE,
                 checksum bytea NOT NULL CHECK (octet_length(checksum) = 32),
                 applied_at timestamptz NOT NULL DEFAULT now()
             )",
        )
        .expect("migration ledger creates");
    for (version, name, sql) in migrations {
        client
            .execute(
                "INSERT INTO telchar_schema_migrations (version, name, checksum) VALUES ($1, $2, $3)",
                &[&version, &name, &Sha256::digest(sql.as_bytes()).to_vec()],
            )
            .expect("prior migration ledger persists");
    }
    client
        .batch_execute(
            "INSERT INTO build_requests (request_id, derivation_path, system, queue_state, queued_at, audit_subject, quota_subject)
             VALUES ('migration-reconciliation-request', '/nix/store/11111111111111111111111111111111-reconciliation.drv', 'x86_64-linux', 'dispatching', transaction_timestamp(), 'test-audit', 'test-quota');
             INSERT INTO execution_attempts (attempt_id, request_id, ordinal, idempotency_key, backend, state)
             VALUES ('migration-reconciliation-attempt', 'migration-reconciliation-request', 1, 'migration-reconciliation-request:1', 'local', 'dispatching');",
        )
        .expect("execution rows persist");
    drop(client);

    let outcome =
        telchar::persistence::migrate(fixture.url()).expect("reconciliation migration applies");

    assert_eq!(outcome.previously_applied, 3);
    assert_eq!(outcome.applied_this_run, 2);
    assert_eq!(outcome.resulting_version, 5);
    let mut client = fixture.connect();
    assert_eq!(
        client
            .query_one(
                "SELECT queue_state FROM build_requests WHERE request_id = 'migration-reconciliation-request'",
                &[],
            )
            .expect("request state reads")
            .get::<_, String>(0),
        "dispatching"
    );
    assert_eq!(
        client
            .query_one(
                "SELECT state FROM execution_attempts WHERE attempt_id = 'migration-reconciliation-attempt'",
                &[],
            )
            .expect("attempt state reads")
            .get::<_, String>(0),
        "dispatching"
    );
    let ledger = client
        .query_one(
            "SELECT name, octet_length(checksum) FROM telchar_schema_migrations WHERE version = 4",
            &[],
        )
        .expect("reconciliation ledger reads");
    assert_eq!(ledger.get::<_, String>(0), "reconciliation_state");
    assert_eq!(ledger.get::<_, i32>(1), 32);
}

#[test]
fn output_retention_migration_backfills_version_one_rows() {
    let fixture = PostgresFixture::start();
    let version_one_sql = include_str!("../migrations/0001_minimum_lifecycle.sql");
    let version_one_checksum = Sha256::digest(version_one_sql.as_bytes()).to_vec();
    let mut client = fixture.connect();
    client
        .batch_execute(version_one_sql)
        .expect("version one schema migrates");
    client
        .batch_execute(
            "CREATE TABLE telchar_schema_migrations (
                 version bigint PRIMARY KEY,
                 name text NOT NULL UNIQUE,
                 checksum bytea NOT NULL CHECK (octet_length(checksum) = 32),
                 applied_at timestamptz NOT NULL DEFAULT now()
             )",
        )
        .expect("migration ledger creates");
    client
        .execute(
            "INSERT INTO telchar_schema_migrations (version, name, checksum) VALUES (1, 'minimum_lifecycle', $1)",
            &[&version_one_checksum],
        )
        .expect("version one ledger persists");
    client
        .batch_execute(
            "INSERT INTO build_requests (request_id, derivation_path, system) VALUES
             ('migration-output-owner', '/nix/store/11111111111111111111111111111111-migration.drv', 'x86_64-linux');
             INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at) VALUES
             ('migration-active-output', 'request', 'migration-output-owner', '/nix/store/22222222222222222222222222222222-active-output', 'output', 'active', transaction_timestamp(), NULL),
             ('migration-released-output', 'request', 'migration-output-owner', '/nix/store/33333333333333333333333333333333-released-output', 'output', 'released', transaction_timestamp(), transaction_timestamp()),
             ('migration-active-input', 'request', 'migration-output-owner', '/nix/store/44444444444444444444444444444444-active-input', 'input', 'active', transaction_timestamp(), NULL);",
        )
        .expect("version one rows persist");
    drop(client);

    let outcome = telchar::persistence::migrate(fixture.url()).expect("version two migrates");

    assert_eq!(outcome.previously_applied, 1);
    assert_eq!(outcome.applied_this_run, 4);
    let mut client = fixture.connect();
    let active_seconds = client
        .query_one(
            "SELECT extract(epoch FROM (expires_at - created_at))::bigint FROM store_leases WHERE lease_id = 'migration-active-output'",
            &[],
        )
        .expect("active deadline reads")
        .get::<_, i64>(0);
    assert_eq!(active_seconds, 3_600);
    let released = client
        .query_one(
            "SELECT expires_at IS NOT NULL, expires_at <= transaction_timestamp() FROM store_leases WHERE lease_id = 'migration-released-output'",
            &[],
        )
        .expect("released deadline reads");
    assert!(released.get::<_, bool>(0));
    assert!(released.get::<_, bool>(1));
    assert!(client
        .query_one(
            "SELECT expires_at IS NULL FROM store_leases WHERE lease_id = 'migration-active-input'",
            &[],
        )
        .expect("input deadline reads")
        .get::<_, bool>(0));
}

#[test]
fn execution_state_migration_upgrades_gate_three_rows() {
    let fixture = PostgresFixture::start();
    let version_one_sql = include_str!("../migrations/0001_minimum_lifecycle.sql");
    let version_two_sql = include_str!("../migrations/0002_output_retention.sql");
    let version_one_checksum = Sha256::digest(version_one_sql.as_bytes()).to_vec();
    let version_two_checksum = Sha256::digest(version_two_sql.as_bytes()).to_vec();
    let mut client = fixture.connect();
    client
        .batch_execute(version_one_sql)
        .expect("version one schema migrates");
    client
        .batch_execute(version_two_sql)
        .expect("version two schema migrates");
    client
        .batch_execute(
            "CREATE TABLE telchar_schema_migrations (
                 version bigint PRIMARY KEY,
                 name text NOT NULL UNIQUE,
                 checksum bytea NOT NULL CHECK (octet_length(checksum) = 32),
                 applied_at timestamptz NOT NULL DEFAULT now()
             )",
        )
        .expect("migration ledger creates");
    client
        .execute(
            "INSERT INTO telchar_schema_migrations (version, name, checksum) VALUES
             (1, 'minimum_lifecycle', $1),
             (2, 'output_retention', $2)",
            &[&version_one_checksum, &version_two_checksum],
        )
        .expect("Gate 3 ledger persists");
    client
        .batch_execute(
            "INSERT INTO protocol_sessions (session_id, requester_reference, state) VALUES
             ('gate-three-session', 'f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e', 'open');
             INSERT INTO build_requests (request_id, derivation_path, system) VALUES
             ('gate-three-request', '/nix/store/11111111111111111111111111111111-migration.drv', 'x86_64-linux');
             INSERT INTO request_attachments (session_id, request_id, state) VALUES
             ('gate-three-session', 'gate-three-request', 'attached');
             INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state) VALUES
             ('gate-three-lease', 'request', 'gate-three-request', '/nix/store/11111111111111111111111111111111-migration.drv', 'derivation', 'active');",
        )
        .expect("Gate 3 rows persist");
    let postgres_version = client
        .query_one("SHOW server_version_num", &[])
        .expect("PostgreSQL version reads")
        .get::<_, String>(0);
    drop(client);

    let outcome = telchar::persistence::migrate(fixture.url()).expect("execution state migrates");

    assert!(postgres_version.parse::<u32>().expect("numeric version") >= 14_00_00);
    assert_eq!(outcome.previously_applied, 2);
    assert_eq!(outcome.applied_this_run, 3);
    assert_eq!(outcome.resulting_version, 5);
    let mut client = fixture.connect();
    let ledger = client
        .query_one(
            "SELECT name, octet_length(checksum), applied_at IS NOT NULL
             FROM telchar_schema_migrations WHERE version = 3",
            &[],
        )
        .expect("execution migration ledger reads");
    assert_eq!(ledger.get::<_, String>(0), "execution_state");
    assert_eq!(ledger.get::<_, i32>(1), 32);
    assert!(ledger.get::<_, bool>(2));
    for table in [
        "execution_attempts",
        "execution_outcomes",
        "capacity_reservations",
    ] {
        assert_eq!(
            client
                .query_one("SELECT to_regclass($1)::text", &[&table])
                .expect("table lookup succeeds")
                .get::<_, Option<String>>(0)
                .as_deref(),
            Some(table)
        );
    }
    let request = client
        .query_one(
            "SELECT derivation_path, system, queue_state, queued_at, audit_subject, quota_subject
             FROM build_requests WHERE request_id = 'gate-three-request'",
            &[],
        )
        .expect("preserved request reads");
    assert_eq!(
        request.get::<_, String>(0),
        "/nix/store/11111111111111111111111111111111-migration.drv"
    );
    assert_eq!(request.get::<_, String>(1), "x86_64-linux");
    assert_eq!(request.get::<_, String>(2), "completed");
    assert!(request.get::<_, Option<SystemTime>>(3).is_none());
    assert_eq!(request.get::<_, String>(4), "gate-three");
    assert_eq!(request.get::<_, String>(5), "gate-three");
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM protocol_sessions WHERE session_id = 'gate-three-session'",
                &[],
            )
            .expect("session count reads")
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM request_attachments WHERE request_id = 'gate-three-request'",
                &[],
            )
            .expect("attachment count reads")
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = 'gate-three-request'",
                &[],
            )
            .expect("lease count reads")
            .get::<_, i64>(0),
        1
    );
    for table in [
        "execution_attempts",
        "execution_outcomes",
        "capacity_reservations",
    ] {
        assert_eq!(
            client
                .query_one(&format!("SELECT count(*) FROM {table}"), &[])
                .expect("execution table count reads")
                .get::<_, i64>(0),
            0
        );
    }
}

#[test]
fn rerunning_an_exact_prefix_is_idempotent() {
    let fixture = PostgresFixture::start();

    let first = telchar::persistence::migrate(fixture.url()).expect("first migration succeeds");
    let second = telchar::persistence::migrate(fixture.url()).expect("second migration succeeds");

    assert_eq!(first.previously_applied, 0);
    assert_eq!(first.applied_this_run, 5);
    assert_eq!(second.previously_applied, 5);
    assert_eq!(second.applied_this_run, 0);
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM telchar_schema_migrations", &[])
            .expect("ledger count reads")
            .get::<_, i64>(0),
        5
    );
}

#[test]
fn altered_applied_checksum_is_rejected() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    fixture
        .connect()
        .execute(
            "UPDATE telchar_schema_migrations SET checksum = decode(repeat('00', 32), 'hex') WHERE version = 1",
            &[],
        )
        .expect("checksum alters");

    let error = telchar::persistence::migrate(fixture.url()).expect_err("checksum rejects");

    assert_eq!(
        error.failure(),
        telchar::persistence::MigrationFailure::Checksum
    );
    assert_eq!(error.to_string(), "database migration failed");
}

#[test]
fn future_schema_version_is_rejected() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    fixture
        .connect()
        .execute(
            "INSERT INTO telchar_schema_migrations (version, name, checksum) VALUES (6, 'future', decode(repeat('00', 32), 'hex'))",
            &[],
        )
        .expect("future migration inserts");

    let error = telchar::persistence::migrate(fixture.url()).expect_err("future version rejects");

    assert_eq!(
        error.failure(),
        telchar::persistence::MigrationFailure::FutureVersion
    );
}

#[test]
fn altered_applied_name_and_applied_gap_are_rejected() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    fixture
        .connect()
        .execute(
            "UPDATE telchar_schema_migrations SET name = 'wrong-name' WHERE version = 1",
            &[],
        )
        .expect("name alters");
    assert_eq!(
        telchar::persistence::migrate(fixture.url())
            .expect_err("name rejects")
            .failure(),
        telchar::persistence::MigrationFailure::Ledger
    );

    let gap = PostgresFixture::start();
    gap.connect()
        .batch_execute(
            "CREATE TABLE telchar_schema_migrations (version bigint PRIMARY KEY, name text NOT NULL UNIQUE, checksum bytea NOT NULL CHECK (octet_length(checksum) = 32), applied_at timestamptz NOT NULL DEFAULT now()); INSERT INTO telchar_schema_migrations (version, name, checksum) VALUES (2, 'gap', decode(repeat('00', 32), 'hex'))",
        )
        .expect("gap inserts");
    assert_eq!(
        telchar::persistence::migrate(gap.url())
            .expect_err("gap rejects")
            .failure(),
        telchar::persistence::MigrationFailure::Ledger
    );
}

#[test]
fn schema_and_ledger_survive_a_database_restart() {
    let mut fixture = PostgresFixture::start();
    let first = telchar::persistence::migrate(fixture.url()).expect("first migration succeeds");

    fixture.restart();

    let second = telchar::persistence::migrate(fixture.url()).expect("second migration succeeds");
    assert_eq!(first.applied_this_run, 5);
    assert_eq!(second.applied_this_run, 0);
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM telchar_schema_migrations", &[])
            .expect("ledger count reads")
            .get::<_, i64>(0),
        5
    );
}

#[test]
fn concurrent_runners_apply_the_migration_once() {
    let fixture = Arc::new(PostgresFixture::start());
    let first_url = fixture.url().to_owned();
    let second_url = fixture.url().to_owned();
    let first = thread::spawn(move || telchar::persistence::migrate(&first_url));
    let second = thread::spawn(move || telchar::persistence::migrate(&second_url));

    let outcomes = [
        first
            .join()
            .expect("first runner does not panic")
            .expect("first runner succeeds"),
        second
            .join()
            .expect("second runner does not panic")
            .expect("second runner succeeds"),
    ];

    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| outcome.applied_this_run)
            .sum::<usize>(),
        5
    );
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM telchar_schema_migrations", &[])
            .expect("ledger count reads")
            .get::<_, i64>(0),
        5
    );
}

#[test]
fn minimum_schema_enforces_domain_constraints() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let mut client = fixture.connect();

    client
        .batch_execute(
            "INSERT INTO protocol_sessions (session_id, requester_reference, state) VALUES ('session', 'requester', 'open');
             INSERT INTO build_requests (request_id, derivation_path, system) VALUES ('request', '/nix/store/test.drv', 'x86_64-linux');
             INSERT INTO request_attachments (session_id, request_id, state) VALUES ('session', 'request', 'attached');
             INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state) VALUES ('lease', 'session', 'session', '/nix/store/path', 'build', 'active');",
        )
        .expect("representative rows insert");

    for statement in [
        "INSERT INTO build_requests (request_id, derivation_path, system) VALUES ('', '/nix/store/test.drv', 'x86_64-linux')",
        "INSERT INTO protocol_sessions (session_id, requester_reference, state) VALUES ('closed', 'requester', 'closed')",
        "DELETE FROM protocol_sessions WHERE session_id = 'session'",
    ] {
        assert!(client.batch_execute(statement).is_err(), "{statement}");
    }
}

#[test]
fn request_input_lease_batch_is_atomic_and_typed() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "input-batch-request",
        "/nix/store/11111111111111111111111111111111-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");

    let records = telchar::persistence::create_request_input_leases(
        fixture.url(),
        "input-batch-request",
        &[
            (
                "input-batch-1".to_owned(),
                "/nix/store/22222222222222222222222222222222-input-a".to_owned(),
            ),
            (
                "input-batch-2".to_owned(),
                "/nix/store/33333333333333333333333333333333-input-b".to_owned(),
            ),
        ],
    )
    .expect("input lease batch persists");

    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| {
        record.owner_kind == telchar::persistence::StoreLeaseOwnerKind::Request
            && record.owner_id == "input-batch-request"
            && record.purpose == telchar::persistence::StoreLeasePurpose::Input
            && record.state == telchar::persistence::StoreLeaseState::Active
            && record.released_at.is_none()
    }));
}

#[test]
fn request_input_lease_batch_rejects_duplicate_paths_before_connection() {
    let error = telchar::persistence::create_request_input_leases(
        "postgresql://invalid",
        "input-batch-request",
        &[
            (
                "input-batch-1".to_owned(),
                "/nix/store/22222222222222222222222222222222-input-a".to_owned(),
            ),
            (
                "input-batch-2".to_owned(),
                "/nix/store/22222222222222222222222222222222-input-a".to_owned(),
            ),
        ],
    )
    .expect_err("duplicate path rejects");
    assert_eq!(
        error.failure(),
        telchar::persistence::StoreLeaseFailure::Configuration
    );
}

#[test]
fn request_input_lease_batch_rolls_back_statement_failure() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "input-statement-failure",
        "/nix/store/11111111111111111111111111111111-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let mut client = fixture.connect();
    client
        .batch_execute(
            "CREATE FUNCTION reject_input_lease() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.lease_id = 'input-failure-2' THEN RAISE EXCEPTION 'reject input'; END IF; RETURN NEW; END $$;
             CREATE TRIGGER reject_input_lease BEFORE INSERT ON store_leases FOR EACH ROW EXECUTE FUNCTION reject_input_lease();",
        )
        .expect("failure trigger installs");

    let error = telchar::persistence::create_request_input_leases(
        fixture.url(),
        "input-statement-failure",
        &[
            (
                "input-failure-1".to_owned(),
                "/nix/store/22222222222222222222222222222222-input-a".to_owned(),
            ),
            (
                "input-failure-2".to_owned(),
                "/nix/store/33333333333333333333333333333333-input-b".to_owned(),
            ),
        ],
    )
    .expect_err("statement failure rejects");
    assert_eq!(
        error.failure(),
        telchar::persistence::StoreLeaseFailure::Query
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = 'input-statement-failure'",
                &[],
            )
            .expect("lease count reads")
            .get::<_, i64>(0),
        0
    );
}

#[test]
fn request_input_lease_batch_rolls_back_deferred_commit_failure() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "input-commit-failure",
        "/nix/store/11111111111111111111111111111111-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let mut client = fixture.connect();
    client
        .batch_execute(
            "CREATE FUNCTION reject_input_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject input commit'; END $$;
             CREATE CONSTRAINT TRIGGER reject_input_commit AFTER INSERT ON store_leases DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_input_commit();",
        )
        .expect("commit trigger installs");

    let error = telchar::persistence::create_request_input_leases(
        fixture.url(),
        "input-commit-failure",
        &[(
            "input-commit-1".to_owned(),
            "/nix/store/22222222222222222222222222222222-input-a".to_owned(),
        )],
    )
    .expect_err("commit failure rejects");
    assert_eq!(
        error.failure(),
        telchar::persistence::StoreLeaseFailure::Commit
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = 'input-commit-failure'",
                &[],
            )
            .expect("lease count reads")
            .get::<_, i64>(0),
        0
    );
}

#[test]
fn create_request_output_leases_commits_complete_ordered_set() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "output-batch-request",
        "/nix/store/11111111111111111111111111111111-output-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");

    let records = telchar::persistence::create_request_output_leases(
        fixture.url(),
        "output-batch-request",
        Duration::from_secs(3_600),
        &[
            (
                "output-batch-2".to_owned(),
                "/nix/store/22222222222222222222222222222222-output-b".to_owned(),
            ),
            (
                "output-batch-1".to_owned(),
                "/nix/store/33333333333333333333333333333333-output-a".to_owned(),
            ),
        ],
    )
    .expect("output lease batch persists");

    assert_eq!(
        records
            .iter()
            .map(|record| record.lease_id.as_str())
            .collect::<Vec<_>>(),
        vec!["output-batch-2", "output-batch-1"]
    );
    assert!(records.iter().all(|record| {
        record.owner_kind == telchar::persistence::StoreLeaseOwnerKind::Request
            && record.owner_id == "output-batch-request"
            && record.purpose == telchar::persistence::StoreLeasePurpose::Output
            && record.state == telchar::persistence::StoreLeaseState::Active
            && record.released_at.is_none()
            && record.expires_at.is_some()
    }));
    assert_eq!(records[0].created_at, records[1].created_at);
    assert_eq!(records[0].expires_at, records[1].expires_at);
    assert_eq!(
        records[0]
            .expires_at
            .expect("output deadline exists")
            .duration_since(records[0].created_at)
            .expect("deadline follows creation"),
        Duration::from_secs(3_600)
    );
}

#[test]
fn create_request_output_leases_rejects_fractional_or_out_of_range_retention() {
    for retention in [
        Duration::from_secs(59),
        Duration::new(60, 1),
        Duration::from_secs(86_401),
    ] {
        assert_eq!(
            telchar::persistence::create_request_output_leases(
                "postgresql://127.0.0.1:1/no-connection",
                "output-duration-request",
                retention,
                &[(
                    "output-duration-lease".to_owned(),
                    "/nix/store/22222222222222222222222222222222-output-duration".to_owned(),
                )],
            )
            .expect_err("invalid retention rejects before connection")
            .failure(),
            telchar::persistence::StoreLeaseFailure::Configuration
        );
    }
}

#[test]
fn create_request_output_leases_rolls_back_second_row_conflict() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    for request_id in ["output-conflict-request", "output-existing-request"] {
        telchar::persistence::create_build_request(
            fixture.url(),
            request_id,
            "/nix/store/11111111111111111111111111111111-output-test.drv",
            "x86_64-linux",
            "test-audit",
            "test-quota",
        )
        .expect("request persists");
    }
    let existing = telchar::persistence::create_store_lease(
        fixture.url(),
        "output-conflict-2",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "output-existing-request",
        "/nix/store/44444444444444444444444444444444-existing-output",
        telchar::persistence::StoreLeasePurpose::Output,
    )
    .expect("existing lease persists");

    let error = telchar::persistence::create_request_output_leases(
        fixture.url(),
        "output-conflict-request",
        Duration::from_secs(3_600),
        &[
            (
                "output-conflict-1".to_owned(),
                "/nix/store/22222222222222222222222222222222-output-a".to_owned(),
            ),
            (
                "output-conflict-2".to_owned(),
                "/nix/store/33333333333333333333333333333333-output-b".to_owned(),
            ),
        ],
    )
    .expect_err("second output lease conflicts");

    assert_eq!(
        error.failure(),
        telchar::persistence::StoreLeaseFailure::Conflict
    );
    assert!(
        telchar::persistence::read_store_lease(fixture.url(), "output-conflict-1")
            .expect("first output lease reads")
            .is_none()
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "output-conflict-2")
            .expect("existing output lease reads"),
        Some(existing)
    );
}

#[test]
fn create_request_output_leases_rejects_invalid_batches_before_mutation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "output-validation-request",
        "/nix/store/11111111111111111111111111111111-output-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let valid = (
        "output-validation-1".to_owned(),
        "/nix/store/22222222222222222222222222222222-output-a".to_owned(),
    );
    let too_many = (0..=nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_OUTPUTS)
        .map(|index| {
            (
                format!("output-limit-{index}"),
                format!("/nix/store/{index:032x}-output-limit-{index}"),
            )
        })
        .collect::<Vec<_>>();

    for leases in [
        vec![valid.clone(), valid.clone()],
        vec![
            valid.clone(),
            ("output-validation-2".to_owned(), valid.1.clone()),
        ],
        vec![(
            "".to_owned(),
            "/nix/store/33333333333333333333333333333333-output-b".to_owned(),
        )],
        vec![("output-invalid-path".to_owned(), "relative".to_owned())],
        too_many,
    ] {
        assert_eq!(
            telchar::persistence::create_request_output_leases(
                fixture.url(),
                "output-validation-request",
                Duration::from_secs(3_600),
                &leases,
            )
            .expect_err("invalid output lease batch rejects")
            .failure(),
            telchar::persistence::StoreLeaseFailure::Configuration
        );
    }
    assert_eq!(
        telchar::persistence::create_request_output_leases(
            fixture.url(),
            "missing-output-request",
            Duration::from_secs(3_600),
            &[valid],
        )
        .expect_err("missing request rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::Missing
    );
    assert_eq!(
        fixture
            .connect()
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = 'output-validation-request'",
                &[],
            )
            .expect("lease count reads")
            .get::<_, i64>(0),
        0
    );
}

#[test]
fn release_expired_output_leases_obeys_deadline_cursor_bound_and_state() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "expiry-request",
        "/nix/store/11111111111111111111111111111111-expiry.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_request_output_leases(
        fixture.url(),
        "expiry-request",
        Duration::from_secs(60),
        &[
            (
                "expiry-a".to_owned(),
                "/nix/store/22222222222222222222222222222222-expiry-a".to_owned(),
            ),
            (
                "expiry-b".to_owned(),
                "/nix/store/33333333333333333333333333333333-expiry-b".to_owned(),
            ),
            (
                "expiry-c".to_owned(),
                "/nix/store/44444444444444444444444444444444-expiry-c".to_owned(),
            ),
        ],
    )
    .expect("output leases persist");
    let now = std::time::SystemTime::now() + Duration::from_secs(61);

    let first =
        telchar::persistence::release_expired_request_output_leases(fixture.url(), now, None, 1)
            .expect("first expiry page releases");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].lease_id, "expiry-a");
    assert_eq!(
        first[0].state,
        telchar::persistence::StoreLeaseState::Released
    );
    assert!(first[0].released_at.is_some());
    assert!(first[0].expires_at.is_some());

    let second = telchar::persistence::release_expired_request_output_leases(
        fixture.url(),
        now,
        Some("expiry-a"),
        256,
    )
    .expect("second expiry page releases");
    assert_eq!(
        second
            .iter()
            .map(|lease| lease.lease_id.as_str())
            .collect::<Vec<_>>(),
        vec!["expiry-b", "expiry-c"]
    );
    assert!(telchar::persistence::release_expired_request_output_leases(
        fixture.url(),
        now,
        None,
        256,
    )
    .expect("released rows are not selected again")
    .is_empty());
}

#[test]
fn release_expired_output_leases_rejects_invalid_inputs_before_connection() {
    let now = std::time::SystemTime::now();
    for (cursor, maximum_rows) in [(None, 0), (None, 257), (Some(""), 1)] {
        assert_eq!(
            telchar::persistence::release_expired_request_output_leases(
                "postgresql://127.0.0.1:1/no-connection",
                now,
                cursor,
                maximum_rows,
            )
            .expect_err("invalid expiry input rejects")
            .failure(),
            telchar::persistence::StoreLeaseFailure::Configuration
        );
    }
}

#[test]
fn create_request_output_leases_empty_set_avoids_database_and_redacts_telemetry() {
    assert!(telchar::persistence::create_request_output_leases(
        "postgresql://127.0.0.1:1/no-connection",
        "output-empty-request",
        Duration::from_secs(3_600),
        &[],
    )
    .expect("empty output lease batch succeeds")
    .is_empty());

    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "output-telemetry-request",
        "/nix/store/11111111111111111111111111111111-output-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let captured = EventCapture::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(captured.clone()));
    tracing::dispatcher::with_default(&dispatch, || {
        telchar::persistence::create_request_output_leases(
            fixture.url(),
            "output-telemetry-request",
            Duration::from_secs(3_600),
            &[(
                "output-telemetry-lease".to_owned(),
                "/nix/store/22222222222222222222222222222222-output-telemetry".to_owned(),
            )],
        )
        .expect("output lease batch persists");
        let error = telchar::persistence::create_request_output_leases(
            "postgresql://output-telemetry-sensitive-url",
            "output-telemetry-sensitive-request",
            Duration::from_secs(3_600),
            &[(
                "output-telemetry-sensitive-lease".to_owned(),
                "relative".to_owned(),
            )],
        )
        .expect_err("invalid output lease batch rejects");
        assert_eq!(
            error.failure(),
            telchar::persistence::StoreLeaseFailure::Configuration
        );
    });
    let events = captured.events();
    assert!(events.iter().any(|event| {
        event.contains("database.store_lease.created")
            && event.contains("operation=\"create-output-retention\"")
            && event.contains("result=\"succeeded\"")
    }));
    for forbidden in [
        fixture.url(),
        "output-telemetry-request",
        "output-telemetry-lease",
        "/nix/store/22222222222222222222222222222222-output-telemetry",
        "output-telemetry-sensitive-url",
        "output-telemetry-sensitive-request",
        "output-telemetry-sensitive-lease",
    ] {
        assert!(
            !events.iter().any(|event| event.contains(forbidden)),
            "{events:?}"
        );
    }
}

#[test]
fn create_request_output_leases_rolls_back_deferred_commit_failure() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "output-commit-failure-request",
        "/nix/store/11111111111111111111111111111111-output-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let mut client = fixture.connect();
    client
        .batch_execute(
            "CREATE FUNCTION reject_output_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.purpose = 'output' THEN RAISE EXCEPTION 'reject output commit'; END IF; RETURN NEW; END $$;
             CREATE CONSTRAINT TRIGGER reject_output_commit AFTER INSERT ON store_leases DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_output_commit();",
        )
        .expect("commit failure trigger installs");

    assert_eq!(
        telchar::persistence::create_request_output_leases(
            fixture.url(),
            "output-commit-failure-request",
            Duration::from_secs(3_600),
            &[(
                "output-commit-failure".to_owned(),
                "/nix/store/22222222222222222222222222222222-output-a".to_owned(),
            )],
        )
        .expect_err("commit failure rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::Commit
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = 'output-commit-failure-request'",
                &[],
            )
            .expect("lease count reads")
            .get::<_, i64>(0),
        0
    );
}

#[test]
fn request_lease_release_rejects_missing_derivation_and_mixed_state_without_mutation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "release-invalid-session",
        requester,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    for (request_id, derivation_lease, input_lease) in [
        (
            "release-missing-derivation",
            None,
            Some("release-missing-input"),
        ),
        (
            "release-mixed-state",
            Some("release-mixed-derivation"),
            Some("release-mixed-input"),
        ),
    ] {
        telchar::persistence::create_build_request(
            fixture.url(),
            request_id,
            "/nix/store/11111111111111111111111111111111-release-invalid.drv",
            "x86_64-linux",
            "test-audit",
            "test-quota",
        )
        .expect("request persists");
        telchar::persistence::attach_request(fixture.url(), "release-invalid-session", request_id)
            .expect("request attaches");
        if let Some(derivation_lease) = derivation_lease {
            telchar::persistence::create_store_lease(
                fixture.url(),
                derivation_lease,
                telchar::persistence::StoreLeaseOwnerKind::Request,
                request_id,
                "/nix/store/11111111111111111111111111111111-release-invalid.drv",
                telchar::persistence::StoreLeasePurpose::Derivation,
            )
            .expect("derivation persists");
        }
        if let Some(input_lease) = input_lease {
            telchar::persistence::create_store_lease(
                fixture.url(),
                input_lease,
                telchar::persistence::StoreLeaseOwnerKind::Request,
                request_id,
                "/nix/store/22222222222222222222222222222222-release-invalid-input",
                telchar::persistence::StoreLeasePurpose::Input,
            )
            .expect("input persists");
        }
    }
    telchar::persistence::release_store_lease(fixture.url(), "release-mixed-input")
        .expect("input changes to released");

    for request_id in ["release-missing-derivation", "release-mixed-state"] {
        assert_eq!(
            telchar::persistence::detach_request_and_release_leases(
                fixture.url(),
                "release-invalid-session",
                request_id,
            )
            .expect_err("invalid request lease set rejects")
            .failure(),
            telchar::persistence::StoreLeaseFailure::Query
        );
        assert_eq!(
            telchar::persistence::read_request_attachment(
                fixture.url(),
                "release-invalid-session",
                request_id,
            )
            .expect("attachment reads")
            .expect("attachment exists")
            .state,
            telchar::persistence::RequestAttachmentState::Attached
        );
    }
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "release-mixed-derivation")
            .expect("derivation reads")
            .expect("derivation exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
}

#[test]
fn request_lease_release_preserves_active_output_leases() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "release-output-session",
        requester,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "release-output-request",
        "/nix/store/11111111111111111111111111111111-release-output.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::attach_request(
        fixture.url(),
        "release-output-session",
        "release-output-request",
    )
    .expect("request attaches");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "release-output-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "release-output-request",
        "/nix/store/11111111111111111111111111111111-release-output.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "release-output-request",
        &[(
            "release-output-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-release-output-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::create_request_output_leases(
        fixture.url(),
        "release-output-request",
        Duration::from_secs(3_600),
        &[(
            "release-output-result".to_owned(),
            "/nix/store/33333333333333333333333333333333-release-output-result".to_owned(),
        )],
    )
    .expect("output lease persists");

    let released = telchar::persistence::detach_request_and_release_leases(
        fixture.url(),
        "release-output-session",
        "release-output-request",
    )
    .expect("request detaches and releasable leases release");

    assert_eq!(
        released
            .leases
            .iter()
            .map(|lease| lease.purpose)
            .collect::<Vec<_>>(),
        vec![
            telchar::persistence::StoreLeasePurpose::Derivation,
            telchar::persistence::StoreLeasePurpose::Input,
        ]
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "release-output-result")
            .expect("output lease reads")
            .expect("output lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "release-output-session",
            "release-output-request",
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Detached
    );
}

#[test]
fn request_lease_release_commit_failure_keeps_attachment_and_leases_active() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "release-commit-session",
        requester,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "release-commit-request",
        "/nix/store/11111111111111111111111111111111-release-commit.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::attach_request(
        fixture.url(),
        "release-commit-session",
        "release-commit-request",
    )
    .expect("request attaches");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "release-commit-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "release-commit-request",
        "/nix/store/11111111111111111111111111111111-release-commit.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");
    fixture
        .connect()
        .batch_execute(
            "CREATE FUNCTION reject_request_release_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject request release commit'; END $$; CREATE CONSTRAINT TRIGGER reject_request_release_commit AFTER UPDATE ON store_leases DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_request_release_commit();",
        )
        .expect("commit failure trigger installs");

    assert_eq!(
        telchar::persistence::detach_request_and_release_leases(
            fixture.url(),
            "release-commit-session",
            "release-commit-request",
        )
        .expect_err("commit rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::Commit
    );
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "release-commit-session",
            "release-commit-request",
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Attached
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "release-commit-derivation")
            .expect("lease reads")
            .expect("lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
}

#[test]
fn request_lease_release_rejects_statement_failure_without_mutation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "release-failure-session",
        requester,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "release-failure-request",
        "/nix/store/11111111111111111111111111111111-release-failure.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::attach_request(
        fixture.url(),
        "release-failure-session",
        "release-failure-request",
    )
    .expect("request attaches");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "release-failure-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "release-failure-request",
        "/nix/store/11111111111111111111111111111111-release-failure.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");
    fixture
        .connect()
        .batch_execute(
            "CREATE FUNCTION reject_request_lease_release() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject request lease release'; END $$; CREATE TRIGGER reject_request_lease_release BEFORE UPDATE ON store_leases FOR EACH ROW EXECUTE FUNCTION reject_request_lease_release();",
        )
        .expect("failure trigger installs");

    assert_eq!(
        telchar::persistence::detach_request_and_release_leases(
            fixture.url(),
            "release-failure-session",
            "release-failure-request",
        )
        .expect_err("lease update rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::Query
    );
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "release-failure-session",
            "release-failure-request",
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Attached
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "release-failure-derivation")
            .expect("lease reads")
            .expect("lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
}

#[test]
fn request_lease_release_page_is_bounded_keyset_ordered_and_includes_output_leases() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "released-page-request",
        "/nix/store/11111111111111111111111111111111-released-page.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "released-page-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "released-page-request",
        "/nix/store/11111111111111111111111111111111-released-page.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation persists");
    let inputs = (0..256)
        .map(|index| {
            (
                format!("released-page-input-{index:03}"),
                format!("/nix/store/{index:032x}-released-page-input-{index:03}"),
            )
        })
        .collect::<Vec<_>>();
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "released-page-request",
        &inputs,
    )
    .expect("inputs persist");
    telchar::persistence::release_unattached_request_leases(fixture.url(), "released-page-request")
        .expect("request leases release");
    telchar::persistence::create_build_request(
        fixture.url(),
        "released-page-other",
        "/nix/store/ffffffffffffffffffffffffffffffff-released-page-other",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("other request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "released-page-output",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "released-page-other",
        "/nix/store/ffffffffffffffffffffffffffffffff-released-page-other",
        telchar::persistence::StoreLeasePurpose::Output,
    )
    .expect("output persists");
    telchar::persistence::release_store_lease(fixture.url(), "released-page-output")
        .expect("output releases");

    let first = telchar::persistence::read_released_request_leases_page(fixture.url(), None, 999)
        .expect("first page reads");
    assert_eq!(first.len(), 256);
    assert!(first
        .windows(2)
        .all(|window| window[0].lease_id < window[1].lease_id));
    assert!(first.iter().all(|lease| {
        lease.owner_kind == telchar::persistence::StoreLeaseOwnerKind::Request
            && lease.state == telchar::persistence::StoreLeaseState::Released
    }));
    let last = first.last().expect("first page has rows");
    let second = telchar::persistence::read_released_request_leases_page(
        fixture.url(),
        Some(&last.lease_id),
        256,
    )
    .expect("second page reads");
    assert_eq!(second.len(), 2);
    assert!(second[0].lease_id > last.lease_id);
    assert!(second[0].lease_id < second[1].lease_id);
    assert!(second.iter().any(|lease| {
        lease.lease_id == "released-page-output"
            && lease.purpose == telchar::persistence::StoreLeasePurpose::Output
    }));
}

#[test]
fn released_request_lease_page_includes_output_reconciliation_authority() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "released-output-page-request",
        "/nix/store/11111111111111111111111111111111-released-output-page.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_request_output_leases(
        fixture.url(),
        "released-output-page-request",
        Duration::from_secs(60),
        &[(
            "released-output-page".to_owned(),
            "/nix/store/22222222222222222222222222222222-released-output-page".to_owned(),
        )],
    )
    .expect("output lease persists");
    telchar::persistence::release_store_lease(fixture.url(), "released-output-page")
        .expect("output lease releases");

    let released =
        telchar::persistence::read_released_request_leases_page(fixture.url(), None, 256)
            .expect("released page reads");

    assert_eq!(released.len(), 1);
    assert_eq!(released[0].lease_id, "released-output-page");
    assert_eq!(
        released[0].purpose,
        telchar::persistence::StoreLeasePurpose::Output
    );
    assert_eq!(
        released[0].state,
        telchar::persistence::StoreLeaseState::Released
    );
}

#[test]
fn request_lease_release_unattached_releases_only_without_attachment() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "unattached-release-request",
        "/nix/store/11111111111111111111111111111111-unattached-release.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "unattached-release-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "unattached-release-request",
        "/nix/store/11111111111111111111111111111111-unattached-release.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");

    let released = telchar::persistence::release_unattached_request_leases(
        fixture.url(),
        "unattached-release-request",
    )
    .expect("unattached lease releases");
    assert_eq!(released.leases.len(), 1);
    assert_eq!(
        released.leases[0].state,
        telchar::persistence::StoreLeaseState::Released
    );

    telchar::persistence::create_build_request(
        fixture.url(),
        "attached-release-request",
        "/nix/store/22222222222222222222222222222222-attached-release.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("attached request persists");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "attached-release-session",
        "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::attach_request(
        fixture.url(),
        "attached-release-session",
        "attached-release-request",
    )
    .expect("request attaches");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "attached-release-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "attached-release-request",
        "/nix/store/22222222222222222222222222222222-attached-release.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");

    assert_eq!(
        telchar::persistence::release_unattached_request_leases(
            fixture.url(),
            "attached-release-request",
        )
        .expect_err("attachment blocks unattached release")
        .failure(),
        telchar::persistence::StoreLeaseFailure::InvalidState
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "attached-release-derivation")
            .expect("lease reads")
            .expect("lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
}

#[test]
fn request_lease_release_detaches_and_releases_complete_active_set_atomically() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "release-session",
        requester,
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "release-request",
        "/nix/store/11111111111111111111111111111111-release.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::attach_request(fixture.url(), "release-session", "release-request")
        .expect("request attaches");
    for (lease_id, store_path, purpose) in [
        (
            "release-derivation",
            "/nix/store/11111111111111111111111111111111-release.drv",
            telchar::persistence::StoreLeasePurpose::Derivation,
        ),
        (
            "release-input",
            "/nix/store/22222222222222222222222222222222-release-input",
            telchar::persistence::StoreLeasePurpose::Input,
        ),
    ] {
        telchar::persistence::create_store_lease(
            fixture.url(),
            lease_id,
            telchar::persistence::StoreLeaseOwnerKind::Request,
            "release-request",
            store_path,
            purpose,
        )
        .expect("lease persists");
    }

    let released = telchar::persistence::detach_request_and_release_leases(
        fixture.url(),
        "release-session",
        "release-request",
    )
    .expect("complete request lease set releases");

    assert_eq!(released.leases.len(), 2);
    assert!(released.leases.iter().all(|lease| {
        lease.state == telchar::persistence::StoreLeaseState::Released
            && lease.released_at.is_some()
    }));
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "release-session",
            "release-request",
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Detached
    );
}
