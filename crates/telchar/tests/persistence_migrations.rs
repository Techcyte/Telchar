//! Tests migration ledger, schema, and backfill behavior contracts and failure boundaries.

mod support;

use std::sync::Arc;
use std::thread;

use postgres::types::Type;
use sha2::{Digest, Sha256};
use support::postgres::PostgresFixture;

#[test]
fn latest_migration_version_matches_resulting_schema() {
    let fixture = PostgresFixture::start();

    let outcome = telchar::persistence::migrate(fixture.url()).expect("migration succeeds");

    assert_eq!(
        telchar::persistence::latest_migration_version(),
        outcome.resulting_version
    );
    assert_eq!(outcome.resulting_version, 16);
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
    assert_eq!(ledger.len(), 16);
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
    assert_eq!(ledger[5].get::<_, i64>(0), 6);
    assert_eq!(ledger[5].get::<_, String>(1), "local_backend_results");
    assert_eq!(ledger[5].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[6].get::<_, i64>(0), 7);
    assert_eq!(
        ledger[6].get::<_, String>(1),
        "protocol_session_credentials"
    );
    assert_eq!(ledger[6].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[7].get::<_, i64>(0), 8);
    assert_eq!(ledger[7].get::<_, String>(1), "retained_store_paths");
    assert_eq!(ledger[7].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[8].get::<_, i64>(0), 9);
    assert_eq!(ledger[8].get::<_, String>(1), "shared_builds");
    assert_eq!(ledger[8].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[9].get::<_, i64>(0), 10);
    assert_eq!(ledger[9].get::<_, String>(1), "shared_build_scheduling");
    assert_eq!(ledger[9].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[10].get::<_, i64>(0), 11);
    assert_eq!(ledger[10].get::<_, String>(1), "shared_build_scheduler");
    assert_eq!(ledger[10].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[11].get::<_, i64>(0), 12);
    assert_eq!(ledger[11].get::<_, String>(1), "shared_build_attempts");
    assert_eq!(ledger[11].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[12].get::<_, i64>(0), 13);
    assert_eq!(ledger[12].get::<_, String>(1), "shared_build_authority");
    assert_eq!(ledger[12].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[13].get::<_, i64>(0), 14);
    assert_eq!(ledger[13].get::<_, String>(1), "nomad_callback_nonces");
    assert_eq!(ledger[13].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[14].get::<_, i64>(0), 15);
    assert_eq!(ledger[14].get::<_, String>(1), "shared_build_specification");
    assert_eq!(ledger[14].get::<_, Vec<u8>>(2).len(), 32);
    assert_eq!(ledger[15].get::<_, i64>(0), 16);
    assert_eq!(ledger[15].get::<_, String>(1), "singleton_ownership");
    assert_eq!(ledger[15].get::<_, Vec<u8>>(2).len(), 32);

    for table in [
        "protocol_sessions",
        "build_requests",
        "request_attachments",
        "store_leases",
        "local_backend_executions",
        "local_backend_execution_results",
        "shared_builds",
        "shared_build_scheduler_state",
        "shared_build_attempts",
        "shared_build_attempt_outcomes",
        "nomad_callback_nonces",
        "singleton_ownership",
    ] {
        let row = client
            .query_one("SELECT to_regclass($1)::text", &[&table])
            .expect("table lookup succeeds");
        assert_eq!(row.get::<_, Option<String>>(0).as_deref(), Some(table));
    }

    for table in [
        "execution_attempts",
        "execution_outcomes",
        "capacity_reservations",
    ] {
        let row = client
            .query_one("SELECT to_regclass($1)::text", &[&table])
            .expect("removed table lookup succeeds");
        assert_eq!(row.get::<_, Option<String>>(0), None);
    }
    for column in ["queue_state", "queued_at"] {
        assert_eq!(
            client
                .query_one(
                    "SELECT count(*) FROM information_schema.columns WHERE table_name = 'build_requests' AND column_name = $1",
                    &[&column],
                )
                .expect("removed column lookup succeeds")
                .get::<_, i64>(0),
            0
        );
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
fn shared_build_attempt_migration_backfills_active_builds() {
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
        (
            4_i64,
            "reconciliation_state",
            include_str!("../migrations/0004_reconciliation_state.sql"),
        ),
        (
            5_i64,
            "local_backend_registry",
            include_str!("../migrations/0005_local_backend_registry.sql"),
        ),
        (
            6_i64,
            "local_backend_results",
            include_str!("../migrations/0006_local_backend_results.sql"),
        ),
        (
            7_i64,
            "protocol_session_credentials",
            include_str!("../migrations/0007_protocol_session_credentials.sql"),
        ),
        (
            8_i64,
            "retained_store_paths",
            include_str!("../migrations/0008_retained_store_paths.sql"),
        ),
        (
            9_i64,
            "shared_builds",
            include_str!("../migrations/0009_shared_builds.sql"),
        ),
        (
            10_i64,
            "shared_build_scheduling",
            include_str!("../migrations/0010_shared_build_scheduling.sql"),
        ),
        (
            11_i64,
            "shared_build_scheduler",
            include_str!("../migrations/0011_shared_build_scheduler.sql"),
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
        .execute(
            "INSERT INTO shared_builds (
                 derivation_path, request_digest, state, backend_name, backend_kind,
                 execution_recovery, cancellation, log_recovery, expected_outputs,
                 started_at, quota_subject, queue_position, queued_at
             ) VALUES (
                 '/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-active.drv',
                 decode(repeat('11', 32), 'hex'), 'running', 'local', 'local',
                 'output-only', 'connection-bound', 'live-only',
                 ARRAY['/nix/store/ffffffffffffffffffffffffffffffff-output'],
                 transaction_timestamp(), 'alice', 1, transaction_timestamp()
             )",
            &[],
        )
        .expect("active shared build persists");
    drop(client);

    let outcome = telchar::persistence::migrate(fixture.url()).expect("attempt migration applies");

    assert_eq!(outcome.previously_applied, 11);
    assert_eq!(outcome.applied_this_run, 5);
    let attempt = telchar::persistence::read_shared_build_attempt(
        fixture.url(),
        "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-active.drv",
    )
    .expect("backfilled attempt reads")
    .expect("backfilled attempt exists");
    assert_eq!(attempt.ordinal, 1);
    assert_eq!(attempt.backend_name, "local");
    assert_eq!(
        attempt.state,
        telchar::persistence::SharedBuildAttemptState::Running
    );
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
    assert_eq!(outcome.applied_this_run, 15);
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
fn rerunning_an_exact_prefix_is_idempotent() {
    let fixture = PostgresFixture::start();

    let first = telchar::persistence::migrate(fixture.url()).expect("first migration succeeds");
    let second = telchar::persistence::migrate(fixture.url()).expect("second migration succeeds");

    assert_eq!(first.previously_applied, 0);
    assert_eq!(first.applied_this_run, 16);
    assert_eq!(second.previously_applied, 16);
    assert_eq!(second.applied_this_run, 0);
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM telchar_schema_migrations", &[])
            .expect("ledger count reads")
            .get::<_, i64>(0),
        16
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
            "INSERT INTO telchar_schema_migrations (version, name, checksum) VALUES (17, 'future', decode(repeat('00', 32), 'hex'))",
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
    assert_eq!(first.applied_this_run, 16);
    assert_eq!(second.applied_this_run, 0);
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM telchar_schema_migrations", &[])
            .expect("ledger count reads")
            .get::<_, i64>(0),
        16
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
        16
    );
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM telchar_schema_migrations", &[])
            .expect("ledger count reads")
            .get::<_, i64>(0),
        16
    );
}
