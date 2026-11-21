//! Tests lease-detail pruning against concurrent request consumers.

use super::*;

#[test]
fn pruning_skips_request_with_pending_root_creation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    let mut cursor = telchar::persistence::LeaseDetailCursor::default();
    let prune = |cursor: &mut telchar::persistence::LeaseDetailCursor| {
        telchar::persistence::prune_store_lease_details(
            fixture.database(),
            Duration::from_secs(30 * 86400),
            256,
            cursor,
        )
        .unwrap()
    };
    let mut client = fixture.connect();
    client.batch_execute("
        INSERT INTO build_requests (request_id, derivation_path, system)
        VALUES ('prune-race', '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-race.drv', 'x86_64-linux');
        INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, nar_size, created_at, released_at, reconciled_at)
        VALUES ('prune-race', 'request', 'prune-race', '/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input', 'input', 'reconciled', 1,
            now() - interval '60 days', now() - interval '50 days', now() - interval '40 days');
    ").unwrap();
    let mut transaction = client.transaction().unwrap();
    transaction.query_one("SELECT request_id FROM build_requests WHERE request_id = 'prune-race' FOR NO KEY UPDATE", &[]).unwrap();
    assert_eq!(prune(&mut cursor), 0);
    transaction.execute("INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, nar_size) VALUES ('pending-root', 'request', 'prune-race', '/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input', 'input', 'active', 1)", &[]).unwrap();
    transaction.commit().unwrap();
    assert_eq!(prune(&mut cursor), 0);
    assert!(
        telchar::persistence::read_store_lease(fixture.database(), "prune-race")
            .unwrap()
            .is_some()
    );
}
