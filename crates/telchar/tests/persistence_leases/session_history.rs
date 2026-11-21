//! Tests disposal of unreferenced closed session details.

use super::*;

#[test]
fn closed_sessions_require_age_and_no_remaining_consumers() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    let mut client = fixture.connect();
    client.batch_execute("
        INSERT INTO protocol_sessions (session_id, requester_reference, state, created_at, closed_at)
        VALUES ('eligible-session','fixture','closed',now() - interval '60 days',now() - interval '40 days'),
               ('recent-session','fixture','closed',now() - interval '60 days',now() - interval '1 day'),
               ('attached-session','fixture','closed',now() - interval '60 days',now() - interval '40 days'),
               ('rooted-session','fixture','closed',now() - interval '60 days',now() - interval '40 days'),
               ('open-session','fixture','open',now() - interval '60 days',NULL);
        INSERT INTO build_requests (request_id,derivation_path,system)
        VALUES ('session-consumer','/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-session.drv','x86_64-linux');
        INSERT INTO request_attachments (session_id,request_id,state,attached_at,detached_at)
        VALUES ('attached-session','session-consumer','detached',now() - interval '50 days',now());
        INSERT INTO store_leases (lease_id,owner_kind,owner_id,store_path,purpose,state,nar_size)
        VALUES ('session-root','session','rooted-session','/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input','input','active',1);
    ").unwrap();
    telchar::persistence::prune_store_lease_details(
        fixture.database(),
        Duration::from_secs(30 * 86400),
        256,
        &mut Default::default(),
    )
    .unwrap();
    let sessions: Vec<String> = client
        .query(
            "SELECT session_id FROM protocol_sessions ORDER BY session_id",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        sessions,
        [
            "attached-session",
            "open-session",
            "recent-session",
            "rooted-session"
        ]
    );
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM request_attachments", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}
