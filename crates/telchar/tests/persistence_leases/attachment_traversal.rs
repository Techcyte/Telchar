//! Tests independent traversal of attachment owner pairs.

use super::*;

#[test]
fn crossed_owner_order_does_not_starve_attachment_history() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    let mut client = fixture.connect();
    client.batch_execute("
        INSERT INTO protocol_sessions (session_id,requester_reference,state,created_at,closed_at)
        SELECT id,'fixture','closed',now() - interval '60 days',now() - interval '40 days'
        FROM unnest(ARRAY['a-session','b-session']) id;
        INSERT INTO build_requests (request_id,derivation_path,system)
        SELECT id,'/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-history.drv','x86_64-linux'
        FROM unnest(ARRAY['a-request','b-request']) id;
        INSERT INTO request_attachments (session_id,request_id,state,attached_at,detached_at)
        VALUES ('a-session','b-request','detached',now() - interval '50 days',now() - interval '40 days'),
               ('b-session','a-request','detached',now() - interval '50 days',now() - interval '40 days');
    ").unwrap();
    let mut cursor = Default::default();
    for _ in 0..4 {
        telchar::persistence::prune_store_lease_details(
            fixture.database(),
            Duration::from_secs(30 * 86400),
            1,
            &mut cursor,
        )
        .unwrap();
    }
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM request_attachments", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM build_requests", &[])
            .unwrap()
            .get::<_, i64>(0),
        2
    );
}
