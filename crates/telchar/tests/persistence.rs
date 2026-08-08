mod support;

use std::sync::Arc;
use std::thread;

use postgres::types::Type;
use support::postgres::PostgresFixture;

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
