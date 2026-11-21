//! Tests a database-time cutoff shared by every page of a retention sweep.

use super::*;

#[test]
fn sweep_does_not_age_new_candidates_between_pages() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    let mut client = fixture.connect();
    client.batch_execute("
        INSERT INTO build_requests (request_id, derivation_path, system)
        VALUES ('a-old', '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-history.drv', 'x86_64-linux'),
               ('b-recent', '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-history.drv', 'x86_64-linux');
        INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, nar_size, created_at, released_at, reconciled_at)
        SELECT request_id, 'request', request_id, '/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input', 'input', 'reconciled', 1,
            now() - interval '10 seconds', now() - interval '5 seconds',
            CASE WHEN request_id = 'a-old' THEN now() - interval '4 seconds' ELSE now() END
        FROM build_requests;
    ").unwrap();
    let mut cursor = telchar::persistence::LeaseDetailCursor::default();
    assert_eq!(
        telchar::persistence::prune_store_lease_details(
            fixture.database(),
            Duration::from_secs(1),
            1,
            &mut cursor
        )
        .unwrap(),
        1
    );
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(
        telchar::persistence::prune_store_lease_details(
            fixture.database(),
            Duration::from_secs(1),
            1,
            &mut cursor
        )
        .unwrap(),
        0
    );
    assert!(
        telchar::persistence::read_store_lease(fixture.database(), "b-recent")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        telchar::persistence::prune_store_lease_details(
            fixture.database(),
            Duration::from_secs(1),
            1,
            &mut cursor
        )
        .unwrap(),
        1
    );
}
