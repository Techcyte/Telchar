//! Tests bounded disposal of reconciled lease details.

use super::*;

#[test]
fn pruning_waits_for_final_reconciliation_and_preserves_live_owners() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    let mut client = fixture.connect();
    client.batch_execute("
        INSERT INTO protocol_sessions (session_id, requester_reference, state, created_at, closed_at)
        VALUES ('closed', 'fixture', 'closed', now() - interval '60 days', now() - interval '40 days'),
               ('open', 'fixture', 'open', now() - interval '60 days', NULL);
        INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, nar_size, created_at, released_at, reconciled_at)
        SELECT 'eligible-' || n, 'session', 'closed', '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-input', 'input', 'reconciled', 1,
            now() - interval '60 days', now() - interval '50 days', now() - interval '40 days'
        FROM generate_series(1, 3) n;
        INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, nar_size, created_at, released_at, reconciled_at)
        VALUES ('recent-reconciliation', 'session', 'closed', '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-input', 'input', 'reconciled', 1,
            now() - interval '60 days', now() - interval '50 days', now() - interval '1 day'),
            ('open-owner', 'session', 'open', '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-input', 'input', 'reconciled', 1,
            now() - interval '60 days', now() - interval '50 days', now() - interval '40 days'),
            ('unreconciled', 'session', 'closed', '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-input', 'input', 'released', 1,
            now() - interval '60 days', now() - interval '50 days', NULL);
    ").unwrap();
    let retention = Duration::from_secs(30 * 86400);
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .unwrap();
    fixture.expire_singleton_ownership("daemon");
    let replacement = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(
        telchar::persistence::prune_store_lease_details(
            owner.database(),
            retention,
            2,
            &mut Default::default()
        )
        .is_err()
    );
    assert_eq!(
        telchar::persistence::prune_store_lease_details(
            replacement.database(),
            retention,
            2,
            &mut Default::default()
        )
        .unwrap(),
        2
    );
    let reopened = telchar::persistence::Database::connect(fixture.url()).unwrap();
    assert_eq!(
        telchar::persistence::prune_store_lease_details(
            &reopened,
            retention,
            2,
            &mut Default::default()
        )
        .unwrap(),
        1
    );
    assert_eq!(
        telchar::persistence::prune_store_lease_details(
            &reopened,
            retention,
            2,
            &mut Default::default()
        )
        .unwrap(),
        0
    );
    let ids: Vec<String> = client
        .query("SELECT lease_id FROM store_leases ORDER BY lease_id", &[])
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(ids, ["open-owner", "recent-reconciliation", "unreconciled"]);
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM protocol_sessions", &[])
            .unwrap()
            .get::<_, i64>(0),
        2
    );
}
