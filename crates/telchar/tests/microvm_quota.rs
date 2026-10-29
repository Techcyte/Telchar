//! Monthly quota admission survives concurrency and unresolved remote execution.

use std::time::{Duration, SystemTime};
use telchar::backend::BackendKind;
use telchar::persistence as db;
use telchar_test_support::postgres::PostgresFixture;

fn intent(database: &db::Database, number: usize, backend: &str) -> db::MicrovmLaunchIntent {
    let path = format!("/nix/store/{number:032}-quota.drv");
    let kind = BackendKind::Microvm;
    db::claim_shared_build(
        database,
        &path,
        &[number as u8; 32],
        backend,
        kind,
        kind.capabilities(),
        None,
        &["/nix/store/ffffffffffffffffffffffffffffffff-output"],
    )
    .unwrap();
    db::start_shared_build(database, &path).unwrap();
    db::MicrovmLaunchIntent {
        intent_id: format!("quota-{number}"),
        attempt_id: db::read_shared_build_attempt(database, &path)
            .unwrap()
            .unwrap()
            .attempt_id,
        backend_name: backend.into(),
        account_id: "116392764028".into(),
        region: "us-west-2".into(),
        image_arn: "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture".into(),
        image_version: "1".into(),
        vpc_egress_connector_arn: "arn:aws:lambda:us-west-2:116392764028:network-connector:builder"
            .into(),
        configuration_digest: [1; 32],
        request_digest: [2; 32],
        client_token: format!("quota-{number}"),
        credential_verifier: [3; 32],
        admission_cutoff: SystemTime::now() + Duration::from_secs(60),
        maximum_duration_seconds: 600,
    }
}

#[test]
fn monthly_allowance_gates_concurrent_aliases_and_preserves_dispatched_debits() {
    let fixture = PostgresFixture::start();
    let database = db::Database::connect(fixture.url()).unwrap();
    db::migrate(&database).unwrap();
    let first = intent(&database, 1, "arm");
    let second = intent(&database, 2, "alias");
    let quota = db::MicrovmQuotaReservation::vm_seconds(600).unwrap();
    std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            db::reserve_microvm_launch_with_quota(&database, &first, 10, Some(&quota)).unwrap()
        });
        let b = scope.spawn(|| {
            db::reserve_microvm_launch_with_quota(&database, &second, 10, Some(&quota)).unwrap()
        });
        assert_ne!(
            a.join().unwrap(),
            b.join().unwrap(),
            "only one launch fits shared allowance"
        );
    });
    let obligations = db::read_microvm_obligations(&database, 10).unwrap();
    assert_eq!(obligations.len(), 1);
    let winner = if obligations[0].intent_id == first.intent_id {
        &first
    } else {
        &second
    };
    let debits = db::read_microvm_quota_debits(&database, 10).unwrap();
    assert_eq!(debits.len(), 1);
    assert_eq!(debits[0].amount, 600);
    assert_eq!(debits[0].unit, "vm-seconds");
    assert!(!debits[0].dispatched);
    assert!(db::dispatch_microvm_launch(&database, &winner.intent_id).unwrap());
    assert!(db::bind_microvm_launch(&database, &winner.intent_id, "microvm-exact").unwrap());
    assert!(db::record_microvm_termination(&database, &winner.intent_id, "microvm-exact").unwrap());
    let third = intent(&database, 3, "arm");
    assert!(!db::reserve_microvm_launch_with_quota(&database, &third, 10, Some(&quota)).unwrap());
    let reconnected = db::Database::connect(fixture.url()).unwrap();
    assert!(
        !db::reserve_microvm_launch_with_quota(&reconnected, &third, 10, Some(&quota)).unwrap()
    );
}

#[test]
fn enabling_quota_refuses_unaccounted_current_month_execution() {
    let fixture = PostgresFixture::start();
    let database = db::Database::connect(fixture.url()).unwrap();
    db::migrate(&database).unwrap();
    let first = intent(&database, 1, "arm");
    assert!(db::reserve_microvm_launch(&database, &first, 10).unwrap());
    assert!(db::dispatch_microvm_launch(&database, &first.intent_id).unwrap());
    let second = intent(&database, 2, "arm");
    let quota = db::MicrovmQuotaReservation::vm_seconds(3600).unwrap();
    assert!(!db::reserve_microvm_launch_with_quota(&database, &second, 10, Some(&quota)).unwrap());
}

#[test]
fn enabling_quota_retains_prior_month_unaccounted_liability() {
    for terminated in [false, true] {
        let fixture = PostgresFixture::start();
        let database = db::Database::connect(fixture.url()).unwrap();
        db::migrate(&database).unwrap();
        let first = intent(&database, 1, "arm");
        let second = intent(&database, 2, "arm");
        assert!(db::reserve_microvm_launch(&database, &first, 10).unwrap());
        assert!(db::dispatch_microvm_launch(&database, &first.intent_id).unwrap());
        let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(60),
        )
        .unwrap();
        if terminated {
            assert!(
                db::bind_microvm_launch(owner.database(), &first.intent_id, "microvm-exact")
                    .unwrap()
            );
            assert!(
                db::record_microvm_termination(owner.database(), &first.intent_id, "microvm-exact")
                    .unwrap()
            );
        } else {
            assert!(db::cancel_microvm_launch(owner.database(), &first.intent_id).unwrap());
            assert!(
                db::release_microvm_capacity_risk(
                    owner.database(),
                    &db::MicrovmRiskRelease {
                        intent_id: &first.intent_id,
                        provider_id: None,
                        actor_uid: 0,
                        reason: "Accept unknown execution risk",
                        evidence_reference: "test:quota",
                        accept_remote_execution_risk: true,
                    }
                )
                .unwrap()
            );
        }
        fixture
            .connect()
            .batch_execute(
                "UPDATE microvm_launches SET created_at = clock_timestamp() - interval '2 months'",
            )
            .unwrap();
        let quota = db::MicrovmQuotaReservation::vm_seconds(3600).unwrap();
        assert!(
            !db::reserve_microvm_launch_with_quota(owner.database(), &second, 10, Some(&quota))
                .unwrap(),
            "terminated={terminated}"
        );
    }
}

#[test]
fn unresolved_execution_carries_allowance_across_months() {
    let fixture = PostgresFixture::start();
    let database = db::Database::connect(fixture.url()).unwrap();
    db::migrate(&database).unwrap();
    let first = intent(&database, 1, "arm");
    let quota = db::MicrovmQuotaReservation::vm_seconds(600).unwrap();
    assert!(db::reserve_microvm_launch_with_quota(&database, &first, 10, Some(&quota)).unwrap());
    assert!(db::dispatch_microvm_launch(&database, &first.intent_id).unwrap());
    fixture.connect().batch_execute(
        "UPDATE microvm_launches SET created_at = clock_timestamp() - interval '2 months';
         UPDATE microvm_quota_reservations SET dispatched_at = clock_timestamp() - interval '2 months'"
    ).unwrap();
    let second = intent(&database, 2, "arm");
    assert!(!db::reserve_microvm_launch_with_quota(&database, &second, 10, Some(&quota)).unwrap());
    assert!(db::bind_microvm_launch(&database, &first.intent_id, "microvm-exact").unwrap());
    assert!(db::record_microvm_termination(&database, &first.intent_id, "microvm-exact").unwrap());
    assert!(db::reserve_microvm_launch_with_quota(&database, &second, 10, Some(&quota)).unwrap());
}

#[test]
fn dispatch_refuses_reservations_prepared_in_another_month() {
    let fixture = PostgresFixture::start();
    let database = db::Database::connect(fixture.url()).unwrap();
    db::migrate(&database).unwrap();
    let first = intent(&database, 1, "arm");
    let quota = db::MicrovmQuotaReservation::vm_seconds(600).unwrap();
    assert!(db::reserve_microvm_launch_with_quota(&database, &first, 10, Some(&quota)).unwrap());
    fixture
        .connect()
        .batch_execute(
            "UPDATE microvm_launches SET created_at = clock_timestamp() - interval '2 months'",
        )
        .unwrap();
    assert!(!db::dispatch_microvm_launch(&database, &first.intent_id).unwrap());
}

#[test]
fn expired_price_quote_prevents_dispatch_without_blocking_cancellation() {
    let fixture = PostgresFixture::start();
    let database = db::Database::connect(fixture.url()).unwrap();
    db::migrate(&database).unwrap();
    let first = intent(&database, 1, "arm");
    let quota = db::MicrovmQuotaReservation::vm_seconds(600).unwrap();
    assert!(db::reserve_microvm_launch_with_quota(&database, &first, 10, Some(&quota)).unwrap());
    fixture
        .connect()
        .batch_execute(
            "UPDATE microvm_quota_reservations SET unit = 'usd-picodollars',
         pricing_reference = 'AWSLambda/us-west-2/fixture',
         pricing_expires_at = clock_timestamp() - interval '1 second'",
        )
        .unwrap();
    assert!(!db::dispatch_microvm_launch(&database, &first.intent_id).unwrap());
    assert!(db::cancel_microvm_launch(&database, &first.intent_id).unwrap());
    assert!(
        db::read_microvm_quota_debits(&database, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ownership_loss_fences_quota_debits() {
    let fixture = PostgresFixture::start();
    let database = db::Database::connect(fixture.url()).unwrap();
    db::migrate(&database).unwrap();
    let first = intent(&database, 1, "arm");
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(60),
    )
    .unwrap();
    let quota = db::MicrovmQuotaReservation::vm_seconds(600).unwrap();
    fixture.expire_singleton_ownership("daemon");
    assert!(
        db::reserve_microvm_launch_with_quota(owner.database(), &first, 10, Some(&quota)).is_err()
    );
    assert!(
        db::read_microvm_quota_debits(&database, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cancelling_before_dispatch_returns_allowance() {
    let fixture = PostgresFixture::start();
    let database = db::Database::connect(fixture.url()).unwrap();
    db::migrate(&database).unwrap();
    let first = intent(&database, 1, "arm");
    let second = intent(&database, 2, "arm");
    let quota = db::MicrovmQuotaReservation::vm_seconds(600).unwrap();
    assert!(db::reserve_microvm_launch_with_quota(&database, &first, 10, Some(&quota)).unwrap());
    assert!(db::cancel_microvm_launch(&database, &first.intent_id).unwrap());
    assert!(db::reserve_microvm_launch_with_quota(&database, &second, 10, Some(&quota)).unwrap());
}
