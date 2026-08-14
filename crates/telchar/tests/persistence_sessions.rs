//! Tests protocol-session, request, and attachment persistence contracts and failure boundaries.

mod support;

use std::sync::Arc;
use std::thread;

use support::postgres::PostgresFixture;

#[test]
fn open_and_read_protocol_session_persist_requested_state() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";

    let opened = telchar::persistence::open_protocol_session(
        fixture.url(),
        "session-1",
        requester_reference,
        "ssh-pubkey:SHA256:release-builder",
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
    assert_eq!(
        read.credential_id.as_deref(),
        Some("ssh-pubkey:SHA256:release-builder")
    );
    assert_eq!(
        read.authentication_authority,
        Some(telchar::persistence::AuthenticationAuthority::OpenSshPublicKey)
    );
    assert_eq!(read.audit_subject, "release-engineering");
    assert_eq!(read.quota_subject, "build-farm");
    assert_eq!(read.state, telchar::persistence::ProtocolSessionState::Open);
    assert!(read.closed_at.is_none());

    let closed = telchar::persistence::close_protocol_session(fixture.url(), "session-1")
        .expect("session closes");
    assert_eq!(
        closed.credential_id.as_deref(),
        Some("ssh-pubkey:SHA256:release-builder")
    );
    assert_eq!(
        closed.authentication_authority,
        Some(telchar::persistence::AuthenticationAuthority::OpenSshPublicKey)
    );
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
    let long_credential_id =
        "ssh-pubkey:".to_owned() + &"c".repeat(telchar::service::ipc::MAX_IPC_CREDENTIAL_ID_BYTES);
    let long_audit_subject = "a".repeat(telchar::service::ipc::MAX_IPC_COMPONENT_BYTES + 1);
    let long_quota_subject = "q".repeat(telchar::service::ipc::MAX_IPC_CREDENTIAL_ID_BYTES + 1);
    for (credential_id, audit_subject, quota_subject) in [
        ("credential", "audit", "quota"),
        ("ssh-pubkey:", "audit", "quota"),
        ("ssh-cert:", "audit", "quota"),
        (long_credential_id.as_str(), "audit", "quota"),
        ("ssh-pubkey:SHA256:test", "", "quota"),
        ("ssh-pubkey:SHA256:test", "audit", ""),
        (
            "ssh-pubkey:SHA256:test",
            long_audit_subject.as_str(),
            "quota",
        ),
        (
            "ssh-pubkey:SHA256:test",
            "audit",
            long_quota_subject.as_str(),
        ),
    ] {
        assert_eq!(
            telchar::persistence::open_protocol_session(
                "postgresql://127.0.0.1:1/no-connection",
                "bounded-session",
                requester_reference,
                credential_id,
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
fn protocol_session_persists_certificate_authentication_authority() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";

    let session = telchar::persistence::open_protocol_session(
        fixture.url(),
        "certificate-session",
        requester_reference,
        "ssh-cert:9:SHA256:ca:8:build-42",
        "builder",
        "build-farm",
    )
    .expect("certificate session opens");

    assert_eq!(
        session.credential_id.as_deref(),
        Some("ssh-cert:9:SHA256:ca:8:build-42")
    );
    assert_eq!(
        session.authentication_authority,
        Some(telchar::persistence::AuthenticationAuthority::OpenSshCertificate)
    );
}

#[test]
fn create_and_read_build_request_persist_immutable_state() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "request-session",
        "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e",
        "ssh-pubkey:SHA256:test",
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
    let long_audit_subject = "a".repeat(telchar::service::ipc::MAX_IPC_COMPONENT_BYTES + 1);
    let long_quota_subject = "q".repeat(telchar::service::ipc::MAX_IPC_CREDENTIAL_ID_BYTES + 1);
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
fn local_backend_terminal_result_is_atomic_idempotent_and_immutable() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::register_local_backend_execution(
        fixture.url(),
        "local-terminal-result",
        "terminal-result:1",
        &[6_u8; 32],
    )
    .expect("backend execution persists");
    telchar::persistence::record_local_backend_running(fixture.url(), "local-terminal-result")
        .expect("backend execution starts");
    let metadata = serde_json::json!({
        "status": "built",
        "outputs": [{
            "name": "out",
            "path": "/nix/store/33333333333333333333333333333333-output"
        }]
    });

    let completed = telchar::persistence::complete_local_backend_execution(
        fixture.url(),
        "local-terminal-result",
        telchar::persistence::LocalBackendExecutionState::Succeeded,
        "succeeded",
        &metadata,
    )
    .expect("terminal result persists");

    assert_eq!(
        completed.execution.state,
        telchar::persistence::LocalBackendExecutionState::Succeeded
    );
    assert!(completed.execution.completed_at.is_some());
    assert_eq!(completed.result.classification, "succeeded");
    assert_eq!(completed.result.result_metadata, metadata);
    assert_eq!(
        telchar::persistence::complete_local_backend_execution(
            fixture.url(),
            "local-terminal-result",
            telchar::persistence::LocalBackendExecutionState::Succeeded,
            "succeeded",
            &metadata,
        )
        .expect("identical terminal result is idempotent"),
        completed
    );
    assert_eq!(
        telchar::persistence::complete_local_backend_execution(
            fixture.url(),
            "local-terminal-result",
            telchar::persistence::LocalBackendExecutionState::Failed,
            "infrastructure-failure",
            &serde_json::json!({}),
        )
        .expect_err("terminal result cannot be replaced")
        .failure(),
        telchar::persistence::LocalBackendExecutionFailure::Conflict
    );
    assert_eq!(
        telchar::persistence::read_local_backend_execution_result(
            fixture.url(),
            "local-terminal-result"
        )
        .expect("terminal result reads")
        .expect("terminal result exists"),
        completed.result
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "closed-session",
        requester_reference,
        "ssh-pubkey:SHA256:test",
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
            &"x".repeat(telchar::service::ipc::MAX_IPC_COMPONENT_BYTES + 1),
            "request",
        ),
        (fixture.url(), "open-session", ""),
        (
            fixture.url(),
            "open-session",
            &"x".repeat(telchar::service::ipc::MAX_IPC_COMPONENT_BYTES + 1),
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
            &"x".repeat(telchar::service::ipc::MAX_IPC_COMPONENT_BYTES + 1),
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
            &"x".repeat(telchar::service::ipc::MAX_IPC_COMPONENT_BYTES + 1),
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
        "ssh-pubkey:SHA256:test",
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
            "ssh-pubkey:SHA256:test",
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
            &"x".repeat(telchar::service::ipc::MAX_IPC_COMPONENT_BYTES + 1),
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
            "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("open session persists");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "closed-session",
        requester_reference,
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
