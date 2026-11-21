//! Measures sparse closed-session pruning against retained live history.

use super::*;
use std::time::Instant;

#[test]
fn sparse_session_history_prunes_only_eligible_detail() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    let mut client = fixture.connect();
    client.batch_execute("
        INSERT INTO protocol_sessions (session_id,requester_reference,state,created_at)
        SELECT 'live-' || lpad(n::text,6,'0'),'fixture','open',now() - interval '60 days'
        FROM generate_series(1,100000) n;
        INSERT INTO protocol_sessions (session_id,requester_reference,state,created_at,closed_at)
        VALUES ('z-eligible','fixture','closed',now() - interval '60 days',now() - interval '40 days');
        ANALYZE protocol_sessions;
    ").unwrap();
    for row in client.query("EXPLAIN (ANALYZE, BUFFERS) SELECT session_id FROM protocol_sessions s WHERE s.state = 'closed' AND (EXISTS (SELECT 1 FROM store_leases l WHERE l.owner_kind = 'session' AND l.owner_id = s.session_id AND l.state = 'reconciled' AND l.reconciled_at < transaction_timestamp() - interval '30 days') OR (s.closed_at < transaction_timestamp() - interval '30 days' AND NOT EXISTS (SELECT 1 FROM store_leases l WHERE l.owner_kind = 'session' AND l.owner_id = s.session_id))) ORDER BY session_id LIMIT 256 FOR NO KEY UPDATE SKIP LOCKED", &[]).unwrap() {
        println!("{}", row.get::<_,String>(0));
    }
    let start = Instant::now();
    telchar::persistence::prune_store_lease_details(
        fixture.database(),
        Duration::from_secs(30 * 86400),
        256,
        &mut Default::default(),
    )
    .unwrap();
    println!("sparse session pruning elapsed {:?}", start.elapsed());
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM protocol_sessions", &[])
            .unwrap()
            .get::<_, i64>(0),
        100000
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM protocol_sessions WHERE session_id = 'z-eligible'",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}
