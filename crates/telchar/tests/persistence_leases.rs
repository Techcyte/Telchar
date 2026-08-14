//! Tests store-lease, retained-byte, and request-release persistence contracts and failure boundaries.

mod support;

use std::fmt;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use support::postgres::PostgresFixture;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

static TELEMETRY_TESTS: Mutex<()> = Mutex::new(());

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
    let _guard = TELEMETRY_TESTS.lock().expect("telemetry lock holds");
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
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "closed-owner",
        requester_reference,
        "ssh-pubkey:SHA256:test",
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
         ALTER TABLE store_leases DROP CONSTRAINT store_leases_retained_size_check;
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
fn concurrent_retained_byte_admission_counts_unique_paths_once() {
    let fixture = Arc::new(PostgresFixture::start());
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    for request_id in ["retained-first", "retained-second"] {
        telchar::persistence::create_build_request(
            fixture.url(),
            request_id,
            "/nix/store/11111111111111111111111111111111-test.drv",
            "x86_64-linux",
            "test-audit",
            "test-quota",
        )
        .expect("request persists");
    }

    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for (request_id, lease_id, store_path) in [
        (
            "retained-first",
            "retained-first-input",
            "/nix/store/22222222222222222222222222222222-shared",
        ),
        (
            "retained-second",
            "retained-second-input",
            "/nix/store/22222222222222222222222222222222-shared",
        ),
    ] {
        let fixture = Arc::clone(&fixture);
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            barrier.wait();
            telchar::persistence::create_request_input_leases_with_limit(
                fixture.url(),
                request_id,
                6,
                &[(lease_id.to_owned(), store_path.to_owned(), 6)],
            )
        }));
    }
    barrier.wait();
    for worker in workers {
        assert_eq!(
            worker.join().expect("worker does not panic").unwrap().len(),
            1
        );
    }
    assert_eq!(
        fixture
            .connect()
            .query_one(
                "SELECT sum(nar_size)::bigint FROM (SELECT store_path, max(nar_size) AS nar_size FROM store_leases WHERE state = 'active' AND purpose IN ('derivation', 'input') GROUP BY store_path) retained",
                &[],
            )
            .expect("retained byte count reads")
            .get::<_, Option<i64>>(0),
        Some(6)
    );
}

#[test]
fn concurrent_retained_byte_admission_rejects_budget_overflow_atomically() {
    let fixture = Arc::new(PostgresFixture::start());
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    for request_id in ["budget-first", "budget-second"] {
        telchar::persistence::create_build_request(
            fixture.url(),
            request_id,
            "/nix/store/11111111111111111111111111111111-test.drv",
            "x86_64-linux",
            "test-audit",
            "test-quota",
        )
        .expect("request persists");
    }

    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for (request_id, lease_id, store_path) in [
        (
            "budget-first",
            "budget-first-input",
            "/nix/store/22222222222222222222222222222222-first",
        ),
        (
            "budget-second",
            "budget-second-input",
            "/nix/store/33333333333333333333333333333333-second",
        ),
    ] {
        let fixture = Arc::clone(&fixture);
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            barrier.wait();
            telchar::persistence::create_request_input_leases_with_limit(
                fixture.url(),
                request_id,
                6,
                &[(lease_id.to_owned(), store_path.to_owned(), 6)],
            )
        }));
    }
    barrier.wait();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().expect("worker does not panic"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(|error| error.failure())
            .collect::<Vec<_>>(),
        [telchar::persistence::StoreLeaseFailure::Capacity]
    );
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM store_leases", &[])
            .expect("lease count reads")
            .get::<_, i64>(0),
        1
    );
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

    let records = telchar::persistence::create_request_input_leases_with_limit(
        fixture.url(),
        "input-batch-request",
        30,
        &[
            (
                "input-batch-1".to_owned(),
                "/nix/store/22222222222222222222222222222222-input-a".to_owned(),
                10,
            ),
            (
                "input-batch-2".to_owned(),
                "/nix/store/33333333333333333333333333333333-input-b".to_owned(),
                20,
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
    let _guard = TELEMETRY_TESTS.lock().expect("telemetry lock holds");
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
        "ssh-pubkey:SHA256:test",
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
