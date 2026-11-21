//! Tests lease-detail traversal beyond ineligible owners.

use super::*;

#[test]
fn ineligible_owner_page_does_not_starve_later_history() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    let mut client = fixture.connect();
    client.batch_execute("
        INSERT INTO build_requests (request_id, derivation_path, system)
        SELECT 'owner-' || lpad(n::text, 4, '0'), '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-history.drv', 'x86_64-linux'
        FROM generate_series(1, 258) n;
        INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, nar_size, created_at, released_at, reconciled_at)
        SELECT request_id, 'request', request_id, '/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input', 'input', 'reconciled', 1,
            now() - interval '60 days', now() - interval '50 days', now() - interval '40 days'
        FROM build_requests;
        INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, nar_size)
        SELECT 'active-' || request_id, 'request', request_id, '/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input', 'input', 'active', 1
        FROM build_requests WHERE request_id <> 'owner-0258';
    ").unwrap();
    let mut deleted = 0;
    let mut cursor = telchar::persistence::LeaseDetailCursor::default();
    for _ in 0..3 {
        deleted += telchar::persistence::prune_store_lease_details(
            fixture.database(),
            Duration::from_secs(30 * 86400),
            256,
            &mut cursor,
        )
        .unwrap();
    }
    assert_eq!(deleted, 1);
    assert!(
        telchar::persistence::read_store_lease(fixture.database(), "owner-0258")
            .unwrap()
            .is_none()
    );
    assert!(
        telchar::persistence::read_store_lease(fixture.database(), "owner-0001")
            .unwrap()
            .is_some()
    );
}
