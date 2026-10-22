//! Subject admission and retained MicroVM capacity are independent durable gates.

use std::time::{Duration, SystemTime};
use telchar::backend::BackendKind;
use telchar::persistence as db;
use telchar_test_support::postgres::PostgresFixture;

#[test]
fn distinct_builds_preserve_subject_order_and_wait_for_remote_capacity() {
    let fixture = PostgresFixture::start();
    let database = db::Database::connect(fixture.url()).unwrap();
    db::migrate(&database).unwrap();
    let paths = [
        "/nix/store/11111111111111111111111111111111-alice-first.drv",
        "/nix/store/22222222222222222222222222222222-alice-second.drv",
        "/nix/store/33333333333333333333333333333333-bob-first.drv",
    ];
    let kind = BackendKind::Microvm;
    for (index, path) in paths.iter().enumerate() {
        db::claim_shared_build(
            &database,
            path,
            &[index as u8; 32],
            "arm",
            kind,
            kind.capabilities(),
            None,
            &["/nix/store/ffffffffffffffffffffffffffffffff-output"],
        )
        .unwrap();
        db::enqueue_shared_build(&database, path, if index == 2 { "bob" } else { "alice" }, 2)
            .unwrap();
    }
    let next = |subject| {
        db::read_next_queued_shared_build(&database, subject, 16)
            .unwrap()
            .unwrap()
            .derivation_path
    };
    assert_eq!(next(None), paths[0]);
    db::start_queued_shared_build(&database, paths[0], 1).unwrap();
    assert_eq!(
        db::start_queued_shared_build(&database, paths[1], 1)
            .unwrap_err()
            .failure(),
        db::SharedBuildFailure::Quota,
    );
    assert_eq!(next(Some("alice")), paths[2]);
    db::start_queued_shared_build(&database, paths[2], 1).unwrap();
    assert_eq!(next(Some("bob")), paths[1]);
    let intent = |path, name: &str| db::MicrovmLaunchIntent {
        intent_id: name.into(),
        attempt_id: db::read_shared_build_attempt(&database, path)
            .unwrap()
            .unwrap()
            .attempt_id,
        backend_name: "arm".into(),
        account_id: "116392764028".into(),
        region: "us-west-2".into(),
        image_arn: "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture".into(),
        image_version: "1".into(),
        configuration_digest: [1; 32],
        request_digest: [2; 32],
        client_token: name.into(),
        credential_verifier: [3; 32],
        admission_cutoff: SystemTime::now() + Duration::from_secs(60),
        maximum_duration_seconds: 60,
    };
    let alice = intent(paths[0], "alice-launch");
    let bob = intent(paths[2], "bob-launch");
    assert!(db::reserve_microvm_launch(&database, &alice, 1).unwrap());
    assert!(!db::reserve_microvm_launch(&database, &bob, 1).unwrap());
    assert!(db::dispatch_microvm_launch(&database, &alice.intent_id).unwrap());
    assert!(db::bind_microvm_launch(&database, &alice.intent_id, "microvm-alice").unwrap());
    db::complete_shared_build_failure(
        &database,
        paths[0],
        "build-failure",
        &serde_json::json!({"stage": "execute"}),
        Duration::from_secs(3600),
    )
    .unwrap();
    db::start_queued_shared_build(&database, paths[1], 1).unwrap();
    assert!(db::cancel_microvm_launch(&database, &alice.intent_id).unwrap());
    assert!(
        !db::reserve_microvm_launch(&database, &bob, 1).unwrap(),
        "terminal build and cancellation must not release remote capacity"
    );
    let row = fixture
        .connect()
        .query_one(
            "SELECT reservation_released_at IS NULL FROM microvm_launches WHERE intent_id = $1",
            &[&alice.intent_id],
        )
        .unwrap();
    assert!(row.get::<_, bool>(0));
    assert_eq!(
        db::read_shared_build(&database, paths[1])
            .unwrap()
            .unwrap()
            .state,
        db::SharedBuildState::Running
    );
    assert_eq!(
        db::read_shared_build(&database, paths[2])
            .unwrap()
            .unwrap()
            .state,
        db::SharedBuildState::Running
    );
}
