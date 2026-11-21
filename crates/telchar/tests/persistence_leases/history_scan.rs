//! Measures pruning traversal through reconciled lease history.

use super::*;

#[test]
fn owner_scan_pages_through_reconciled_history() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    let mut client = fixture.connect();
    client.batch_execute("
        INSERT INTO build_requests (request_id, derivation_path, system)
        SELECT 'empty-' || lpad(n::text, 6, '0'), '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-history.drv', 'x86_64-linux'
        FROM generate_series(1, 100000) n;
        INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, nar_size, created_at, released_at, reconciled_at)
        SELECT request_id, 'request', request_id, '/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input', 'input', 'reconciled', 1,
            now() - interval '60 days', now() - interval '50 days', now() - interval '40 days'
        FROM build_requests;
        ANALYZE build_requests;
        ANALYZE store_leases;
    ").unwrap();
    for (label, query) in [
        (
            "lease-filtered owners",
            "EXPLAIN (ANALYZE, BUFFERS) SELECT request_id FROM build_requests r WHERE EXISTS (SELECT 1 FROM store_leases l WHERE l.owner_kind = 'request' AND l.owner_id = r.request_id AND l.state = 'reconciled' AND l.reconciled_at < transaction_timestamp() - interval '30 days') ORDER BY request_id LIMIT 256 FOR NO KEY UPDATE SKIP LOCKED",
        ),
        (
            "bounded owners",
            "EXPLAIN (ANALYZE, BUFFERS) SELECT request_id FROM build_requests r ORDER BY request_id LIMIT 256 FOR NO KEY UPDATE SKIP LOCKED",
        ),
    ] {
        println!("{label}");
        for row in client.query(query, &[]).unwrap() {
            println!("{}", row.get::<_, String>(0));
        }
    }
    let mut cursor = telchar::persistence::LeaseDetailCursor::default();
    assert_eq!(
        telchar::persistence::prune_store_lease_details(
            fixture.database(),
            Duration::from_secs(30 * 86400),
            256,
            &mut cursor
        )
        .unwrap(),
        256
    );
}
