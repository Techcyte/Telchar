//! Tests persistence store leases.

use super::*;

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
