use std::time::{Duration, SystemTime};
use telchar::backend::BackendKind;
use telchar::persistence as db;
use telchar_test_support::postgres::PostgresFixture;

#[test]
fn bound_worker_reset_is_fenced_and_launch_close_prevents_assignment() {
    worker_lifecycle(true);
}

#[test]
fn zero_retention_reset_never_publishes_claimable_worker() {
    worker_lifecycle(false);
}

fn worker_lifecycle(retain: bool) {
    let fixture = PostgresFixture::start();
    db::migrate(fixture.url()).unwrap();
    let path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-worker.drv";
    let kind = BackendKind::Microvm;
    db::claim_shared_build(
        fixture.url(),
        path,
        &[1; 32],
        "arm",
        kind,
        kind.capabilities(),
        None,
        &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-output"],
    )
    .unwrap();
    db::enqueue_shared_build(fixture.url(), path, "alice", 1).unwrap();
    db::start_queued_shared_build(fixture.url(), path, 1).unwrap();
    let attempt = db::read_shared_build_attempt(fixture.url(), path)
        .unwrap()
        .unwrap()
        .attempt_id;
    let intent = db::MicrovmLaunchIntent {
        intent_id: "worker-launch".into(),
        attempt_id: attempt,
        backend_name: "arm".into(),
        account_id: "116392764028".into(),
        region: "us-west-2".into(),
        image_arn: "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture".into(),
        image_version: "1".into(),
        vpc_egress_connector_arn: "connector".into(),
        configuration_digest: [1; 32],
        request_digest: [2; 32],
        client_token: "worker-launch".into(),
        credential_verifier: [3; 32],
        admission_cutoff: SystemTime::now() + Duration::from_secs(60),
        maximum_duration_seconds: 120,
    };
    assert!(db::reserve_microvm_launch(fixture.url(), &intent, 1).unwrap());
    assert!(db::dispatch_microvm_launch(fixture.url(), &intent.intent_id).unwrap());
    assert!(db::bind_microvm_launch(fixture.url(), &intent.intent_id, "provider-worker").unwrap());
    let mut client = fixture.connect();
    let row = client
        .query_one(
            "SELECT state, attempt_id FROM microvm_workers WHERE intent_id = $1",
            &[&intent.intent_id],
        )
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "busy");
    assert_eq!(row.get::<_, i64>(1), attempt);
    let capability = telchar::microvm::Capability::generate().unwrap();
    assert!(
        db::authorize_microvm_assignment(
            fixture.url(),
            &intent.intent_id,
            1,
            &capability.verifier()
        )
        .unwrap()
    );
    assert!(!db::admit_microvm_assignment(fixture.url(), &intent.intent_id, 1, &[0; 32]).unwrap());
    assert!(
        db::admit_microvm_assignment(fixture.url(), &intent.intent_id, 1, &capability.verifier())
            .unwrap()
    );
    assert!(
        !db::admit_microvm_assignment(fixture.url(), &intent.intent_id, 1, &capability.verifier())
            .unwrap()
    );
    assert!(!db::reset_microvm_worker(fixture.url(), &intent.intent_id, 1, attempt).unwrap());
    assert!(
        db::completed_microvm_worker(fixture.url(), &intent.intent_id)
            .unwrap()
            .is_none()
    );
    db::collect_shared_build(fixture.url(), path).unwrap();
    db::complete_shared_build_success(
        fixture.url(),
        path,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(
        db::completed_microvm_worker(fixture.url(), &intent.intent_id).unwrap(),
        Some((1, attempt))
    );
    assert!(!db::reset_microvm_worker(fixture.url(), &intent.intent_id, 2, attempt).unwrap());
    if !retain {
        assert!(
            db::reset_and_retire_microvm_worker(fixture.url(), &intent.intent_id, 1, attempt)
                .unwrap()
        );
        assert!(
            !db::reset_and_retire_microvm_worker(fixture.url(), &intent.intent_id, 1, attempt)
                .unwrap()
        );
        assert!(
            !db::claim_microvm_worker(
                fixture.url(),
                &intent.intent_id,
                1,
                attempt,
                Duration::from_secs(10)
            )
            .unwrap()
        );
        let row = client.query_one("SELECT w.state,w.attempt_id IS NULL,s.completed_at IS NOT NULL,l.reservation_released_at IS NULL FROM microvm_workers w JOIN microvm_assignments s USING(intent_id,assignment_epoch) JOIN microvm_launches l USING(intent_id) WHERE intent_id=$1", &[&intent.intent_id]).unwrap();
        assert_eq!(row.get::<_, String>(0), "draining");
        assert!(row.get::<_, bool>(1));
        assert!(row.get::<_, bool>(2));
        assert!(row.get::<_, bool>(3));
        return;
    }
    assert!(db::reset_microvm_worker(fixture.url(), &intent.intent_id, 1, attempt).unwrap());
    assert!(!db::reset_microvm_worker(fixture.url(), &intent.intent_id, 1, attempt).unwrap());
    let assignment = client.query_one("SELECT attempt_id, admitted_at IS NOT NULL, completed_at IS NOT NULL FROM microvm_assignments WHERE intent_id=$1 AND assignment_epoch=1", &[&intent.intent_id]).unwrap();
    assert_eq!(assignment.get::<_, i64>(0), attempt);
    assert!(assignment.get::<_, bool>(1));
    assert!(assignment.get::<_, bool>(2));
    let second = "/nix/store/cccccccccccccccccccccccccccccccc-worker.drv";
    db::claim_shared_build(
        fixture.url(),
        second,
        &[2; 32],
        "arm",
        kind,
        kind.capabilities(),
        None,
        &["/nix/store/dddddddddddddddddddddddddddddddd-output"],
    )
    .unwrap();
    db::enqueue_shared_build(fixture.url(), second, "bob", 1).unwrap();
    db::start_queued_shared_build(fixture.url(), second, 1).unwrap();
    let next = db::read_shared_build_attempt(fixture.url(), second)
        .unwrap()
        .unwrap()
        .attempt_id;
    assert!(
        db::claim_microvm_worker(
            fixture.url(),
            &intent.intent_id,
            1,
            next,
            Duration::from_secs(10)
        )
        .unwrap()
    );
    assert!(
        db::microvm_execution_active(fixture.url(), &intent.intent_id).unwrap(),
        "second assignment must not inherit terminal launch-attempt state"
    );
    assert!(
        db::microvm_session_request(fixture.url(), &intent.intent_id, 1)
            .unwrap()
            .is_none(),
        "previous epoch cannot load later assignment even if request identity matches"
    );
    assert!(
        db::pending_microvm_cleanup(fixture.url(), &intent.account_id, &intent.region, 10, None)
            .unwrap()
            .is_empty(),
        "original terminal attempt cannot schedule current assignment for cleanup"
    );
    assert!(
        !db::pending_microvm_cleanup_identity(fixture.url(), &intent.intent_id, "provider-worker")
            .unwrap(),
        "stale cleanup candidate cannot terminate current assignment"
    );
    assert!(!db::reset_microvm_worker(fixture.url(), &intent.intent_id, 1, next).unwrap());
    db::collect_shared_build(fixture.url(), second).unwrap();
    db::complete_shared_build_success(
        fixture.url(),
        second,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        db::pending_microvm_cleanup_identity(fixture.url(), &intent.intent_id, "provider-worker")
            .unwrap(),
        "terminal current assignment remains eligible for maintenance"
    );
    assert!(db::reset_microvm_worker(fixture.url(), &intent.intent_id, 2, next).unwrap());
    let third = "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-worker.drv";
    db::claim_shared_build(
        fixture.url(),
        third,
        &[3; 32],
        "arm",
        kind,
        kind.capabilities(),
        None,
        &["/nix/store/ffffffffffffffffffffffffffffffff-output"],
    )
    .unwrap();
    db::enqueue_shared_build(fixture.url(), third, "carol", 1).unwrap();
    db::start_queued_shared_build(fixture.url(), third, 1).unwrap();
    let next = db::read_shared_build_attempt(fixture.url(), third)
        .unwrap()
        .unwrap()
        .attempt_id;
    assert!(
        !db::begin_microvm_cleanup(fixture.url(), &intent.intent_id, "provider-worker").unwrap(),
        "retained idle worker cannot be cleaned before deadline"
    );
    client.execute("UPDATE microvm_workers SET idle_until=clock_timestamp()-interval '1 second' WHERE intent_id=$1", &[&intent.intent_id]).unwrap();
    assert!(
        !db::claim_microvm_worker(
            fixture.url(),
            &intent.intent_id,
            2,
            next,
            Duration::from_secs(10)
        )
        .unwrap(),
        "expired idle worker cannot be claimed"
    );
    assert!(
        db::pending_microvm_cleanup_identity(fixture.url(), &intent.intent_id, "provider-worker")
            .unwrap()
    );
    client
        .execute(
            "UPDATE microvm_workers SET idle_until=expires_at WHERE intent_id=$1",
            &[&intent.intent_id],
        )
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let claim_barrier = barrier.clone();
    let url = fixture.url().to_owned();
    let claim = std::thread::spawn(move || {
        claim_barrier.wait();
        db::claim_microvm_worker(&url, "worker-launch", 2, next, Duration::from_secs(10)).unwrap()
    });
    barrier.wait();
    let retired = db::retire_microvm_worker(fixture.url(), &intent.intent_id, 2).unwrap();
    let claimed = claim.join().unwrap();
    assert_ne!(
        claimed, retired,
        "claim and retirement must have exactly one winner"
    );
    if claimed {
        db::collect_shared_build(fixture.url(), third).unwrap();
        db::complete_shared_build_success(
            fixture.url(),
            third,
            &serde_json::json!({}),
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(
            db::reset_and_retire_microvm_worker(fixture.url(), &intent.intent_id, 3, next).unwrap()
        );
        assert!(
            !db::claim_microvm_worker(
                fixture.url(),
                &intent.intent_id,
                3,
                next,
                Duration::from_secs(10)
            )
            .unwrap()
        );
        assert!(!db::retire_microvm_worker(fixture.url(), &intent.intent_id, 1).unwrap());
        assert!(!db::reset_microvm_worker(fixture.url(), &intent.intent_id, 1, next).unwrap());
    }
    assert!(!db::reset_microvm_worker(fixture.url(), &intent.intent_id, 1, attempt).unwrap());
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(60),
    )
    .unwrap();
    let row = client.query_one("SELECT w.state, l.phase, l.reservation_released_at IS NULL FROM microvm_workers w JOIN microvm_launches l USING(intent_id) WHERE intent_id=$1", &[&intent.intent_id]).unwrap();
    assert_eq!(row.get::<_, String>(0), "draining");
    assert_eq!(row.get::<_, String>(1), "closed");
    assert!(row.get::<_, bool>(2));
    assert!(
        !db::claim_microvm_worker(
            owner.database(),
            &intent.intent_id,
            1,
            next,
            Duration::from_secs(10)
        )
        .unwrap()
    );
    assert!(!db::cancel_microvm_launch(owner.database(), &intent.intent_id).unwrap());
    let row = client.query_one("SELECT w.state, l.reservation_released_at IS NULL FROM microvm_workers w JOIN microvm_launches l USING(intent_id) WHERE intent_id=$1", &[&intent.intent_id]).unwrap();
    assert_eq!(row.get::<_, String>(0), "draining");
    assert!(row.get::<_, bool>(1));
}
