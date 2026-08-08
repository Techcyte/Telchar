mod support;

use std::sync::Arc;
use std::thread;

use postgres::types::Type;
use support::postgres::PostgresFixture;

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
    )
    .expect("session opens");
    let read = telchar::persistence::read_protocol_session(fixture.url(), "session-1")
        .expect("session reads")
        .expect("session exists");

    assert_eq!(opened, read);
    assert_eq!(read.session_id, "session-1");
    assert_eq!(read.requester_reference, requester_reference);
    assert_eq!(read.state, telchar::persistence::ProtocolSessionState::Open);
    assert!(read.closed_at.is_none());
}

#[test]
fn duplicate_and_invalid_protocol_session_opens_reject_without_mutation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester_reference = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(fixture.url(), "session-1", requester_reference)
        .expect("first open succeeds");

    for reference in [
        requester_reference,
        "d97fcc940167deddfb3b76c3a5398037de37729288d7111cad50e693000e2ec3",
    ] {
        let error =
            telchar::persistence::open_protocol_session(fixture.url(), "session-1", reference)
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
        let error =
            telchar::persistence::open_protocol_session(fixture.url(), session_id, reference)
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
    )
    .expect("open session persists");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "closed-session",
        requester_reference,
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
    telchar::persistence::open_protocol_session(fixture.url(), "failed-close", requester_reference)
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
fn empty_database_migrates_to_minimum_lifecycle_schema() {
    let fixture = PostgresFixture::start();

    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");

    let mut client = fixture.connect();
    let ledger = client
        .query_one(
            "SELECT version, name, checksum FROM telchar_schema_migrations",
            &[],
        )
        .expect("migration ledger reads");
    assert_eq!(ledger.get::<_, i64>(0), 1);
    assert_eq!(ledger.get::<_, String>(1), "minimum_lifecycle");
    assert_eq!(ledger.get::<_, Vec<u8>>(2).len(), 32);

    for table in [
        "protocol_sessions",
        "build_requests",
        "request_attachments",
        "store_leases",
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
fn rerunning_an_exact_prefix_is_idempotent() {
    let fixture = PostgresFixture::start();

    let first = telchar::persistence::migrate(fixture.url()).expect("first migration succeeds");
    let second = telchar::persistence::migrate(fixture.url()).expect("second migration succeeds");

    assert_eq!(first.previously_applied, 0);
    assert_eq!(first.applied_this_run, 1);
    assert_eq!(second.previously_applied, 1);
    assert_eq!(second.applied_this_run, 0);
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM telchar_schema_migrations", &[])
            .expect("ledger count reads")
            .get::<_, i64>(0),
        1
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
            "INSERT INTO telchar_schema_migrations (version, name, checksum) VALUES (2, 'future', decode(repeat('00', 32), 'hex'))",
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
    assert_eq!(first.applied_this_run, 1);
    assert_eq!(second.applied_this_run, 0);
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM telchar_schema_migrations", &[])
            .expect("ledger count reads")
            .get::<_, i64>(0),
        1
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
        1
    );
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM telchar_schema_migrations", &[])
            .expect("ledger count reads")
            .get::<_, i64>(0),
        1
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
