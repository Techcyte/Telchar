//! Tests protocol sessions.

use super::*;

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
