//! Tests attachments.

use super::*;

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
