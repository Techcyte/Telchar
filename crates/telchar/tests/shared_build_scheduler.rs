//! Tests shared build scheduler contracts and failure boundaries, including claim.

use telchar_test_support::postgres::PostgresFixture;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use telchar::backend::BackendKind;
use telchar::service::config::SchedulingLimits;

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
fn saturated_waiters_share_bounded_subject_examinations() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let active = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-active.drv";
    claim(&fixture, active, 1);
    telchar::persistence::enqueue_shared_build(fixture.url(), active, "alice", 16).unwrap();
    telchar::persistence::start_queued_shared_build(fixture.url(), active, 1).unwrap();
    let paths = (0..8)
        .map(|index| {
            let path = format!("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-waiter-{index}.drv");
            claim(&fixture, &path, 2);
            telchar::persistence::enqueue_shared_build(fixture.url(), &path, "alice", 16).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let examined = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&examined);
    let scheduler = Arc::new(
        telchar::shared_build::scheduler::SharedBuildScheduler::new(
            telchar::persistence::Database::connect(fixture.url()).unwrap(),
            move |_| {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                SchedulingLimits::new(16, 1).unwrap()
            },
        )
        .unwrap(),
    );
    let waiters = paths
        .iter()
        .map(|path| {
            let scheduler = Arc::clone(&scheduler);
            let path = path.clone();
            thread::spawn(move || scheduler.wait_for_admission(&path))
        })
        .collect::<Vec<_>>();
    thread::sleep(Duration::from_millis(600));
    let count = examined.load(std::sync::atomic::Ordering::Relaxed);
    for path in &paths {
        telchar::persistence::complete_shared_build_failure(
            fixture.url(),
            path,
            "fixture-stop",
            &serde_json::json!({}),
            Duration::from_secs(60),
        )
        .unwrap();
    }
    scheduler.capacity_changed();
    for waiter in waiters {
        assert!(waiter.join().unwrap().is_err());
    }
    eprintln!("eight saturated waiters, 600ms: {count} subject examinations");
    assert!(
        count <= 16,
        "blocked waiters repeat scheduling scans: {count}"
    );
}

#[test]
fn scheduler_construction_fails_when_durable_rotation_cannot_be_read() {
    assert!(
        telchar::shared_build::scheduler::SharedBuildScheduler::new(
            telchar::persistence::Database::connect("postgresql://127.0.0.1:1/unavailable")
                .expect("pool config is accepted"),
            |_| SchedulingLimits::new(1, 1).expect("limits are valid"),
        )
        .is_err()
    );
}

#[test]
fn waiting_build_starts_after_subject_capacity_is_released() {
    capacity_release(true);
}

#[test]
fn periodic_recheck_admits_after_missed_capacity_notification() {
    capacity_release(false);
}

fn capacity_release(notify: bool) {
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
        telchar::shared_build::scheduler::SharedBuildScheduler::new(
            telchar::persistence::Database::connect(fixture.url()).expect("database connects"),
            |_| SchedulingLimits::new(2, 1).expect("limits are valid"),
        )
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
    if notify {
        scheduler.capacity_changed();
    }

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

    let scheduler = telchar::shared_build::scheduler::SharedBuildScheduler::new(
        telchar::persistence::Database::connect(fixture.url()).expect("database connects"),
        |_| SchedulingLimits::new(2, 1).expect("limits are valid"),
    )
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

    let restarted = telchar::shared_build::scheduler::SharedBuildScheduler::new(
        telchar::persistence::Database::connect(fixture.url()).expect("database connects"),
        |_| SchedulingLimits::new(2, 1).expect("limits are valid"),
    )
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
fn concurrent_waiters_for_one_subject_are_admitted() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let builds = [
        "/nix/store/66666666666666666666666666666666-first.drv",
        "/nix/store/77777777777777777777777777777777-second.drv",
        "/nix/store/88888888888888888888888888888888-third.drv",
    ];
    for (index, build) in builds.iter().enumerate() {
        claim(&fixture, build, index as u8 + 6);
        telchar::persistence::enqueue_shared_build(fixture.url(), build, "alice", 8)
            .expect("build enqueues");
    }

    let scheduler = Arc::new(
        telchar::shared_build::scheduler::SharedBuildScheduler::new(
            telchar::persistence::Database::connect(fixture.url()).expect("database connects"),
            |_| SchedulingLimits::new(8, 8).expect("limits are valid"),
        )
        .expect("scheduler creates"),
    );
    let waiters = builds.map(|build| {
        let scheduler = Arc::clone(&scheduler);
        thread::spawn(move || scheduler.wait_for_admission(build))
    });

    for waiter in waiters {
        waiter
            .join()
            .expect("waiter does not panic")
            .expect("build is admitted");
    }
}

#[test]
fn saturated_subject_page_does_not_exclude_later_subject() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    for index in 0..257 {
        let path = format!("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-waiting-{index:03}.drv");
        claim(&fixture, &path, 1);
        telchar::persistence::enqueue_shared_build(
            fixture.url(),
            &path,
            &format!("subject-{index:03}"),
            2,
        )
        .unwrap();
        if index < 256 {
            let active =
                format!("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-active-{index:03}.drv");
            claim(&fixture, &active, 2);
            telchar::persistence::enqueue_shared_build(
                fixture.url(),
                &active,
                &format!("subject-{index:03}"),
                2,
            )
            .unwrap();
            telchar::persistence::start_queued_shared_build(fixture.url(), &active, 1).unwrap();
        }
    }
    let scheduler = telchar::shared_build::scheduler::SharedBuildScheduler::new(
        telchar::persistence::Database::connect(fixture.url()).unwrap(),
        |_| SchedulingLimits::new(2, 1).unwrap(),
    )
    .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let waiter =
        thread::spawn(move || {
            sender
                .send(scheduler.wait_for_admission(
                    "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-waiting-256.drv",
                ))
                .unwrap();
        });
    let result = receiver.recv_timeout(Duration::from_secs(10));
    assert!(
        result.is_ok(),
        "later subject starved behind saturated page"
    );
    assert_eq!(
        result.unwrap().unwrap().derivation_path,
        "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-waiting-256.drv"
    );
    waiter.join().unwrap();
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

    let scheduler = telchar::shared_build::scheduler::SharedBuildScheduler::new(
        telchar::persistence::Database::connect(fixture.url()).expect("database connects"),
        |_| SchedulingLimits::new(2, 1).expect("limits are valid"),
    )
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

#[test]
fn ownership_recovery_blocks_queued_dispatch_even_after_database_returns() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = Arc::new(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(10),
        )
        .unwrap(),
    );
    let path = "/nix/store/99999999999999999999999999999999-paused.drv";
    claim(&fixture, path, 9);
    telchar::persistence::enqueue_shared_build(owner.database(), path, "alice", 1).unwrap();
    let scheduler = telchar::shared_build::scheduler::SharedBuildScheduler::new(
        owner.database().clone().with_ownership(&owner),
        |_| SchedulingLimits::new(1, 1).unwrap(),
    )
    .unwrap();
    fixture.stop();
    assert!(owner.renew().is_err());
    fixture.restart();
    let waiting = thread::spawn(move || scheduler.wait_for_admission(path));
    thread::sleep(Duration::from_millis(150));
    assert!(!waiting.is_finished());
    assert_eq!(
        telchar::persistence::read_shared_build(fixture.url(), path)
            .unwrap()
            .unwrap()
            .state,
        telchar::persistence::SharedBuildState::Claimed
    );
    owner.verify().unwrap();
    thread::sleep(Duration::from_millis(50));
    assert!(
        !waiting.is_finished(),
        "read-only verification resumed queued dispatch"
    );
    // Replacing a stale application connection may itself observe uncertainty after
    // the first renewal; keep the real renewal worker active for that observation.
    let renewal = owner.with_renewal(Duration::from_millis(100)).unwrap();
    assert_eq!(
        waiting.join().unwrap().unwrap().state,
        telchar::persistence::SharedBuildState::Running
    );
    drop(renewal);
}
