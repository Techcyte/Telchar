//! Tests request consumers of reconciled lease history.

use super::*;

#[test]
fn request_detail_pruning_preserves_unresolved_consumers() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    let mut client = fixture.connect();
    let prune = || {
        telchar::persistence::prune_store_lease_details(
            fixture.database(),
            Duration::from_secs(30 * 86400),
            256,
            &mut Default::default(),
        )
        .unwrap()
    };
    client.batch_execute("
        INSERT INTO build_requests (request_id, derivation_path, system, created_at)
        SELECT id, '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-' || id || '.drv', 'x86_64-linux', now() - interval '60 days'
        FROM unnest(ARRAY['eligible','retained','unresolved']) id;
        INSERT INTO store_leases (lease_id,owner_kind,owner_id,store_path,purpose,state,nar_size,created_at,released_at,reconciled_at)
        SELECT id, 'request', id, '/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input', 'input', 'reconciled', 1,
            now() - interval '60 days', now() - interval '50 days', now() - interval '40 days'
        FROM unnest(ARRAY['eligible','retained','unresolved']) id;
        INSERT INTO store_leases (lease_id,owner_kind,owner_id,store_path,purpose,state,nar_size)
        VALUES ('active-root','request','retained','/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input','input','active',1);
    ").unwrap();
    let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-unresolved.drv";
    telchar::persistence::claim_shared_build(
        fixture.database(),
        drv,
        &[4; 32],
        "nomad",
        telchar::backend::BackendKind::Nomad,
        telchar::backend::BackendKind::Nomad.capabilities(),
        Some("pruning-job"),
        &["/nix/store/cccccccccccccccccccccccccccccccc-output"],
    )
    .unwrap();
    telchar::persistence::start_shared_build(fixture.database(), drv).unwrap();
    client.execute("INSERT INTO nomad_execution_obligations (attempt_id) SELECT attempt_id FROM shared_build_attempts WHERE derivation_path = $1", &[&drv]).unwrap();
    telchar::persistence::complete_shared_build_failure(
        fixture.database(),
        drv,
        "fixture-failure",
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    client.execute("UPDATE shared_builds SET created_at = now() - interval '60 days', started_at = now() - interval '50 days', completed_at = now() - interval '40 days' WHERE derivation_path = $1", &[&drv]).unwrap();
    assert_eq!(prune(), 1);
    client
        .execute(
            "UPDATE nomad_execution_obligations SET resolved_at = clock_timestamp()",
            &[],
        )
        .unwrap();
    assert_eq!(prune(), 0);
    for id in ["retained", "unresolved", "active-root"] {
        assert!(
            telchar::persistence::read_store_lease(fixture.database(), id)
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM build_requests", &[])
            .unwrap()
            .get::<_, i64>(0),
        3
    );
    assert!(
        telchar::persistence::read_shared_build(fixture.database(), drv)
            .unwrap()
            .is_some()
    );
}
