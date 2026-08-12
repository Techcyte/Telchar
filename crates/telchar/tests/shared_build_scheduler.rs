mod support;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use support::postgres::PostgresFixture;
use telchar::backend::BackendKind;
use telchar::config::SchedulingLimits;

fn claim(fixture: &PostgresFixture, derivation_path: &str, digest: u8) {
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[digest; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/ffffffffffffffffffffffffffffffff-output"],
    )
    .expect("shared build claims");
}

#[test]
fn scheduler_construction_fails_when_durable_rotation_cannot_be_read() {
    assert!(telchar::shared_build_scheduler::SharedBuildScheduler::new(
        "postgresql://127.0.0.1:1/unavailable",
        |_| SchedulingLimits::new(1, 1).expect("limits are valid"),
    )
    .is_err());
}

#[test]
fn waiting_build_starts_after_subject_capacity_is_released() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let first = "/nix/store/11111111111111111111111111111111-first.drv";
    let second = "/nix/store/22222222222222222222222222222222-second.drv";
    claim(&fixture, first, 1);
    claim(&fixture, second, 2);
    telchar::persistence::enqueue_shared_build(fixture.url(), first, "alice", 2)
        .expect("first build enqueues");
    telchar::persistence::enqueue_shared_build(fixture.url(), second, "alice", 2)
        .expect("second build enqueues");

    let scheduler = Arc::new(
        telchar::shared_build_scheduler::SharedBuildScheduler::new(fixture.url(), |_| {
            SchedulingLimits::new(2, 1).expect("limits are valid")
        })
        .expect("scheduler creates"),
    );
    scheduler
        .wait_for_admission(first)
        .expect("first build starts");

    let waiting_scheduler = Arc::clone(&scheduler);
    let waiting = thread::spawn(move || {
        waiting_scheduler
            .wait_for_admission(second)
            .expect("second build starts")
    });
    thread::sleep(Duration::from_millis(100));
    assert!(
        !waiting.is_finished(),
        "second build started above subject limit"
    );

    telchar::persistence::complete_shared_build_failure(
        fixture.url(),
        first,
        "fixture-complete",
        &serde_json::json!({"stage": "test"}),
        Duration::from_secs(60),
    )
    .expect("first build completes");
    scheduler.capacity_changed();

    assert_eq!(
        waiting.join().expect("waiting thread joins").state,
        telchar::persistence::SharedBuildState::Running
    );
}

#[test]
fn subject_rotation_survives_scheduler_restart() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let alice_first = "/nix/store/66666666666666666666666666666666-alice-first.drv";
    let alice_second = "/nix/store/77777777777777777777777777777777-alice-second.drv";
    let bob_first = "/nix/store/88888888888888888888888888888888-bob-first.drv";
    claim(&fixture, alice_first, 6);
    claim(&fixture, alice_second, 7);
    claim(&fixture, bob_first, 8);
    telchar::persistence::enqueue_shared_build(fixture.url(), alice_first, "alice", 2)
        .expect("Alice first enqueues");

    let scheduler =
        telchar::shared_build_scheduler::SharedBuildScheduler::new(fixture.url(), |_| {
            SchedulingLimits::new(2, 1).expect("limits are valid")
        })
        .expect("scheduler creates");
    scheduler
        .wait_for_admission(alice_first)
        .expect("Alice first starts");
    drop(scheduler);
    telchar::persistence::complete_shared_build_failure(
        fixture.url(),
        alice_first,
        "fixture-complete",
        &serde_json::json!({"stage": "test"}),
        Duration::from_secs(60),
    )
    .expect("Alice first completes");
    telchar::persistence::enqueue_shared_build(fixture.url(), alice_second, "alice", 2)
        .expect("Alice second enqueues");
    telchar::persistence::enqueue_shared_build(fixture.url(), bob_first, "bob", 1)
        .expect("Bob first enqueues");

    let restarted =
        telchar::shared_build_scheduler::SharedBuildScheduler::new(fixture.url(), |_| {
            SchedulingLimits::new(2, 1).expect("limits are valid")
        })
        .expect("scheduler restarts");
    let bob = restarted
        .wait_for_admission(bob_first)
        .expect("Bob starts after scheduler restart");
    let alice = telchar::persistence::read_shared_build(fixture.url(), alice_second)
        .expect("Alice second reads")
        .expect("Alice second exists");
    assert!(
        bob.started_at.expect("Bob start time exists")
            <= alice.started_at.expect("Alice start time exists"),
        "persisted rotation must admit Bob before returning to Alice"
    );
}

#[test]
fn saturated_subject_does_not_block_another_subject() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let alice_active = "/nix/store/33333333333333333333333333333333-alice-active.drv";
    let alice_waiting = "/nix/store/44444444444444444444444444444444-alice-waiting.drv";
    let bob_waiting = "/nix/store/55555555555555555555555555555555-bob-waiting.drv";
    claim(&fixture, alice_active, 3);
    claim(&fixture, alice_waiting, 4);
    claim(&fixture, bob_waiting, 5);
    telchar::persistence::enqueue_shared_build(fixture.url(), alice_active, "alice", 2)
        .expect("active Alice build enqueues");
    telchar::persistence::enqueue_shared_build(fixture.url(), alice_waiting, "alice", 2)
        .expect("waiting Alice build enqueues");
    telchar::persistence::enqueue_shared_build(fixture.url(), bob_waiting, "bob", 2)
        .expect("Bob build enqueues");
    telchar::persistence::start_queued_shared_build(fixture.url(), alice_active, 1)
        .expect("Alice build starts");

    let scheduler =
        telchar::shared_build_scheduler::SharedBuildScheduler::new(fixture.url(), |_| {
            SchedulingLimits::new(2, 1).expect("limits are valid")
        })
        .expect("scheduler creates");
    assert_eq!(
        scheduler
            .wait_for_admission(bob_waiting)
            .expect("Bob build starts")
            .derivation_path,
        bob_waiting
    );
    assert_eq!(
        telchar::persistence::read_shared_build(fixture.url(), alice_waiting)
            .expect("Alice build reads")
            .expect("Alice build exists")
            .state,
        telchar::persistence::SharedBuildState::Claimed
    );
}
