//! Tests attachment detail pruning against pending request roots.

use super::*;
use std::time::Instant;

#[test]
fn attachment_pruning_skips_locked_parent_and_preserves_committed_root() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    let mut client = fixture.connect();
    client.batch_execute("
        INSERT INTO protocol_sessions (session_id,requester_reference,state,created_at,closed_at)
        VALUES ('locked-session','fixture','closed',now() - interval '60 days',now() - interval '40 days');
        INSERT INTO build_requests (request_id,derivation_path,system)
        VALUES ('locked-request','/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-history.drv','x86_64-linux');
        INSERT INTO request_attachments (session_id,request_id,state,attached_at,detached_at)
        VALUES ('locked-session','locked-request','detached',now() - interval '50 days',now() - interval '40 days');
    ").unwrap();
    let mut cursor = Default::default();
    let prune = |cursor: &mut telchar::persistence::LeaseDetailCursor| {
        telchar::persistence::prune_store_lease_details(
            fixture.database(),
            Duration::from_secs(30 * 86400),
            1,
            cursor,
        )
        .unwrap();
    };
    let mut transaction = client.transaction().unwrap();
    transaction.query_one("SELECT request_id FROM build_requests WHERE request_id = 'locked-request' FOR NO KEY UPDATE", &[]).unwrap();
    let start = Instant::now();
    prune(&mut cursor);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(
        transaction
            .query_one("SELECT count(*) FROM request_attachments", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    transaction.execute("INSERT INTO store_leases (lease_id,owner_kind,owner_id,store_path,purpose,state,nar_size) VALUES ('pending-root','request','locked-request','/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input','input','active',1)", &[]).unwrap();
    transaction.commit().unwrap();
    for _ in 0..3 {
        prune(&mut cursor);
    }
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM request_attachments", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM protocol_sessions", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}
