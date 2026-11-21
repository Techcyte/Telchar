//! Tests retention of attachment reconciliation evidence.

use super::*;

#[test]
fn attachment_pruning_requires_absent_lease_and_execution_consumers() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    let mut client = fixture.connect();
    client.batch_execute("
        INSERT INTO protocol_sessions (session_id,requester_reference,state,created_at,closed_at)
        SELECT id,'fixture','closed',now() - interval '60 days',now() - interval '40 days'
        FROM unnest(ARRAY['eligible','rooted','recent','executing']) id;
        INSERT INTO build_requests (request_id,derivation_path,system)
        SELECT id,'/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-' || id || '.drv','x86_64-linux'
        FROM unnest(ARRAY['eligible','rooted','recent','executing']) id;
        INSERT INTO request_attachments (session_id,request_id,state,attached_at,detached_at)
        SELECT session_id,session_id,'detached',now() - interval '50 days',
            CASE WHEN session_id = 'recent' THEN now() ELSE now() - interval '40 days' END
        FROM protocol_sessions;
        INSERT INTO store_leases (lease_id,owner_kind,owner_id,store_path,purpose,state,nar_size)
        VALUES ('root','request','rooted','/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input','input','active',1);
    ").unwrap();
    telchar::persistence::claim_shared_build(
        fixture.database(),
        "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-executing.drv",
        &[4; 32],
        "local",
        telchar::backend::BackendKind::Local,
        telchar::backend::BackendKind::Local.capabilities(),
        None,
        &["/nix/store/cccccccccccccccccccccccccccccccc-output"],
    )
    .unwrap();
    telchar::persistence::prune_store_lease_details(
        fixture.database(),
        Duration::from_secs(30 * 86400),
        256,
        &mut Default::default(),
    )
    .unwrap();
    let owners: Vec<String> = client
        .query(
            "SELECT request_id FROM request_attachments ORDER BY request_id",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(owners, ["executing", "recent", "rooted"]);
    let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-executing.drv";
    telchar::persistence::start_shared_build(fixture.database(), drv).unwrap();
    telchar::persistence::complete_shared_build_failure(
        fixture.database(),
        drv,
        "fixture-failure",
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    client.execute("UPDATE shared_builds SET created_at = now() - interval '60 days', started_at = now() - interval '50 days', completed_at = now() - interval '40 days' WHERE derivation_path = $1", &[&drv]).unwrap();
    client.execute("UPDATE shared_build_attempts SET created_at = now() - interval '60 days', started_at = now() - interval '50 days', completed_at = now() - interval '40 days' WHERE derivation_path = $1", &[&drv]).unwrap();
    client.execute("INSERT INTO nomad_execution_obligations (attempt_id, created_at) SELECT attempt_id, now() - interval '50 days' FROM shared_build_attempts WHERE derivation_path = $1", &[&drv]).unwrap();
    let prune = || {
        telchar::persistence::prune_store_lease_details(
            fixture.database(),
            Duration::from_secs(30 * 86400),
            256,
            &mut Default::default(),
        )
        .unwrap()
    };
    let attachments = || {
        fixture
            .connect()
            .query_one(
                "SELECT count(*) FROM request_attachments WHERE request_id = 'executing'",
                &[],
            )
            .unwrap()
            .get::<_, i64>(0)
    };
    prune();
    assert_eq!(attachments(), 1);
    client
        .execute(
            "UPDATE nomad_execution_obligations SET resolved_at = now()",
            &[],
        )
        .unwrap();
    prune();
    assert_eq!(attachments(), 1);
    client
        .execute(
            "UPDATE nomad_execution_obligations SET resolved_at = now() - interval '40 days'",
            &[],
        )
        .unwrap();
    client.execute("INSERT INTO post_build_hooks_outbox (attempt_id,output_paths,state,completed_at) SELECT attempt_id, ARRAY['/nix/store/cccccccccccccccccccccccccccccccc-output'], 'succeeded', now() FROM shared_build_attempts WHERE derivation_path = $1", &[&drv]).unwrap();
    telchar::persistence::prune_post_build_hooks(fixture.database()).unwrap();
    prune();
    assert_eq!(attachments(), 1);
    client
        .execute(
            "UPDATE post_build_hooks_outbox SET completed_at = now() - interval '40 days'",
            &[],
        )
        .unwrap();
    client.execute("INSERT INTO post_build_hook_roots (attempt_id,output_index,store_path,retained,released_at) SELECT attempt_id,0,'/nix/store/cccccccccccccccccccccccccccccccc-output',true,now() FROM shared_build_attempts WHERE derivation_path = $1", &[&drv]).unwrap();
    telchar::persistence::prune_post_build_hooks(fixture.database()).unwrap();
    prune();
    assert_eq!(attachments(), 1);
    client
        .execute(
            "UPDATE post_build_hook_roots SET released_at = now() - interval '40 days'",
            &[],
        )
        .unwrap();
    prune();
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM request_attachments WHERE request_id = 'executing'",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    telchar::persistence::prune_post_build_hooks(fixture.database()).unwrap();
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM post_build_hooks_outbox", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(
        telchar::persistence::read_shared_build(fixture.database(), drv)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM build_requests", &[])
            .unwrap()
            .get::<_, i64>(0),
        4
    );
}
