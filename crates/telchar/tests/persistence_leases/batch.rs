//! Exercises input admission batches and statement counts against PostgreSQL.

use super::*;
use telchar::persistence::{
    self, Database, StoreLeaseFailure, StoreLeaseOwnerKind, StoreLeasePurpose, StoreLeaseState,
};

fn database() -> (PostgresFixture, Database) {
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url()).unwrap();
    (fixture, database)
}

fn request(database: &Database, id: &str) {
    persistence::create_build_request(
        database,
        id,
        &format!("/nix/store/11111111111111111111111111111111-{id}.drv"),
        "x86_64-linux",
        "batch-audit",
        "batch-quota",
    )
    .unwrap();
}

fn leases(prefix: &str, count: usize) -> Vec<(String, String, u64)> {
    (0..count)
        .rev()
        .map(|index| {
            (
                format!("{prefix}-{index}"),
                format!("/nix/store/22222222222222222222222222222222-{prefix}-{index}"),
                index as u64 + 1,
            )
        })
        .collect()
}

fn observe_insert_statements(client: &mut postgres::Client) {
    client.batch_execute(
        "CREATE TABLE lease_statements (count bigint NOT NULL);
         INSERT INTO lease_statements VALUES (0);
         CREATE FUNCTION count_lease_statement() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN UPDATE lease_statements SET count = count + 1; RETURN NULL; END $$;
         CREATE TRIGGER count_lease_statement AFTER INSERT ON store_leases FOR EACH STATEMENT EXECUTE FUNCTION count_lease_statement();"
    ).unwrap();
}

fn statement_count(client: &mut postgres::Client) -> i64 {
    client
        .query_one("SELECT count FROM lease_statements", &[])
        .unwrap()
        .get(0)
}

#[test]
fn input_batch_uses_one_insert_statement() {
    let (fixture, database) = database();
    request(&database, "statement-batch");
    let mut client = fixture.connect();
    observe_insert_statements(&mut client);
    let records = persistence::create_request_input_leases_with_limit(
        &database,
        "statement-batch",
        u64::MAX,
        &leases("statement", 10),
    )
    .unwrap();
    assert_eq!(records.len(), 10);
    assert_eq!(statement_count(&mut client), 1);
}

#[test]
fn empty_single_and_maximum_batches_preserve_input_order_and_metadata() {
    assert!(
        persistence::create_request_input_leases_with_limit("invalid", "", 0, &[])
            .unwrap()
            .is_empty()
    );
    let (_fixture, database) = database();
    for count in [
        1,
        nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES,
    ] {
        let id = format!("ordered-{count}");
        request(&database, &id);
        let entries = leases(&id, count);
        let records =
            persistence::create_request_input_leases_with_limit(&database, &id, u64::MAX, &entries)
                .unwrap();
        assert_eq!(records.len(), entries.len());
        for (record, (lease_id, path, size)) in records.iter().zip(&entries) {
            assert_eq!(&record.lease_id, lease_id);
            assert_eq!(&record.store_path, path);
            assert_eq!(record.nar_size, Some(*size));
            assert_eq!(record.owner_id, id);
            assert_eq!(record.owner_kind, StoreLeaseOwnerKind::Request);
            assert_eq!(record.purpose, StoreLeasePurpose::Input);
            assert_eq!(record.state, StoreLeaseState::Active);
            assert!(record.released_at.is_none());
            assert!(record.expires_at.is_none());
        }
    }
}

#[test]
fn late_conflict_rolls_back_entire_batch() {
    let (fixture, database) = database();
    request(&database, "conflict-owner");
    request(&database, "conflict-batch");
    let occupied = leases("occupied", 1);
    persistence::create_request_input_leases_with_limit(
        &database,
        "conflict-owner",
        u64::MAX,
        &occupied,
    )
    .unwrap();
    let mut entries = leases("conflict", 10);
    entries.last_mut().unwrap().0 = occupied[0].0.clone();
    let error = persistence::create_request_input_leases_with_limit(
        &database,
        "conflict-batch",
        u64::MAX,
        &entries,
    )
    .unwrap_err();
    assert_eq!(error.failure(), StoreLeaseFailure::Conflict);
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM store_leases", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        persistence::read_store_lease(&database, &occupied[0].0)
            .unwrap()
            .unwrap()
            .owner_id,
        "conflict-owner"
    );
}

#[test]
fn mixed_retained_paths_count_once_and_reject_size_conflicts() {
    let (_fixture, database) = database();
    for id in [
        "shared-owner",
        "mixed-batch",
        "overflow-batch",
        "size-conflict",
    ] {
        request(&database, id);
    }
    let shared = leases("shared", 1);
    persistence::create_request_input_leases_with_limit(&database, "shared-owner", 1, &shared)
        .unwrap();
    let mut mixed = leases("mixed", 2);
    mixed.push(("mixed-shared".into(), shared[0].1.clone(), 1));
    let records =
        persistence::create_request_input_leases_with_limit(&database, "mixed-batch", 4, &mixed)
            .unwrap();
    assert_eq!(
        records
            .iter()
            .map(|record| &record.lease_id)
            .collect::<Vec<_>>(),
        mixed.iter().map(|entry| &entry.0).collect::<Vec<_>>()
    );
    let error = persistence::create_request_input_leases_with_limit(
        &database,
        "overflow-batch",
        4,
        &leases("overflow", 1),
    )
    .unwrap_err();
    assert_eq!(error.failure(), StoreLeaseFailure::Capacity);
    let error = persistence::create_request_input_leases_with_limit(
        &database,
        "size-conflict",
        u64::MAX,
        &[("size-conflict".into(), shared[0].1.clone(), 2)],
    )
    .unwrap_err();
    assert_eq!(error.failure(), StoreLeaseFailure::Conflict);
}

#[test]
fn concurrent_batches_cannot_overcommit_retained_capacity() {
    let (fixture, database) = database();
    for id in ["concurrent-a", "concurrent-b"] {
        request(&database, id);
    }
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let workers = ["concurrent-a", "concurrent-b"].map(|id| {
        let database = database.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            persistence::create_request_input_leases_with_limit(&database, id, 6, &leases(id, 3))
        })
    });
    barrier.wait();
    let results = workers.map(|worker| worker.join().unwrap());
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .next()
            .unwrap()
            .failure(),
        StoreLeaseFailure::Capacity
    );
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM store_leases", &[])
            .unwrap()
            .get::<_, i64>(0),
        3
    );
}

#[test]
#[ignore = "release-mode PostgreSQL batch benchmark; no timing assertion"]
fn measure_input_lease_batches() {
    let (fixture, database) = database();
    let mut client = fixture.connect();
    observe_insert_statements(&mut client);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    for count in [10, 100, 1000, 4096] {
        for repetition in 0..5 {
            let id = format!("bench-{nonce}-{count}-{repetition}");
            request(&database, &id);
            let entries = leases(&id, count);
            client
                .batch_execute("TRUNCATE store_leases; ALTER TABLE store_leases DISABLE TRIGGER count_lease_statement; UPDATE lease_statements SET count = 0")
                .unwrap();
            let start = std::time::Instant::now();
            let records = persistence::create_request_input_leases_with_limit(
                &database,
                &id,
                u64::MAX,
                &entries,
            )
            .unwrap();
            let elapsed = start.elapsed();
            assert_eq!(records.len(), count);
            // Statement auditing runs separately so its writes are outside the timed operation.
            client.batch_execute("TRUNCATE store_leases; ALTER TABLE store_leases ENABLE TRIGGER count_lease_statement").unwrap();
            let observed_id = format!("observed-{nonce}-{count}-{repetition}");
            request(&database, &observed_id);
            persistence::create_request_input_leases_with_limit(
                &database,
                &observed_id,
                u64::MAX,
                &leases(&observed_id, count),
            )
            .unwrap();
            let statements = statement_count(&mut client);
            println!(
                "LEASE_BATCH_BENCHMARK {}",
                serde_json::json!({"paths": count, "repetition": repetition, "seconds": elapsed.as_secs_f64(), "insert_statements": statements})
            );
        }
    }
}
