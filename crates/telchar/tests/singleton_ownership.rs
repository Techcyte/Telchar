//! Tests singleton ownership contracts and failure boundaries, including only one owner acquires the database lifetime lock.

use telchar_test_support::postgres::PostgresFixture;

use std::time::Duration;

#[test]
fn tls_owner_acquires_and_renews_the_database_lease() {
    let fixture = PostgresFixture::start_tls();
    telchar::persistence::migrate(fixture.url()).expect("database migrates over TLS");

    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("daemon acquires ownership over TLS");

    owner.renew().expect("daemon renews ownership over TLS");
    owner.verify().expect("daemon verifies ownership over TLS");
    drop(owner);

    telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("replacement acquires released ownership over TLS");
}

#[test]
fn only_one_owner_acquires_the_database_lease() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");

    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("first daemon acquires ownership");
    assert_eq!(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(20),
        )
        .expect_err("second daemon is refused")
        .failure(),
        telchar::service::singleton_ownership::SingletonOwnershipFailure::Contended
    );

    drop(owner);
    telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("replacement acquires after authoritative release");
}

#[test]
fn replacement_acquires_expired_lease_with_higher_generation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("first daemon acquires ownership");
    fixture.expire_singleton_ownership("daemon");

    let replacement = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("replacement acquires expired lease");

    assert!(replacement.generation() > owner.generation());
    assert_eq!(
        owner
            .verify()
            .expect_err("expired owner is fenced")
            .failure(),
        telchar::service::singleton_ownership::SingletonOwnershipFailure::Fenced
    );
    drop(owner);
    replacement
        .verify()
        .expect("retiring owner preserves replacement");
    replacement.renew().expect("replacement remains renewable");
}

#[test]
fn keyword_database_url_is_fenced() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let keyword_url = fixture.keyword_url();

    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        &keyword_url,
        Duration::from_secs(20),
    )
    .expect("keyword database URL acquires ownership");

    telchar::persistence::create_build_request(
        owner.database_url(),
        "keyword-owner-request",
        "/nix/store/00000000000000000000000000000000-keyword.drv",
        "x86_64-linux",
        "keyword-owner",
        "keyword-owner",
    )
    .expect("keyword database URL carries fencing settings");
}

#[test]
fn expired_owner_cannot_mutate_durable_state_after_takeover() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("first daemon acquires ownership");
    let stale_database_url = owner.database_url().to_owned();
    fixture.expire_singleton_ownership("daemon");
    let replacement = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("replacement acquires expired lease");

    assert!(
        telchar::persistence::create_build_request(
            &stale_database_url,
            "stale-owner-request",
            "/nix/store/11111111111111111111111111111111-stale.drv",
            "x86_64-linux",
            "stale-owner",
            "stale-owner",
        )
        .is_err()
    );
    telchar::persistence::create_build_request(
        replacement.database_url(),
        "replacement-request",
        "/nix/store/22222222222222222222222222222222-replacement.drv",
        "x86_64-linux",
        "replacement",
        "replacement",
    )
    .expect("replacement mutates durable state");
}

#[test]
fn renewal_extends_lease_only_for_current_owner() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("daemon acquires ownership");

    owner.renew().expect("current owner renews");
    fixture.expire_singleton_ownership("daemon");
    let _replacement = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("replacement acquires expired lease");

    assert_eq!(
        owner
            .renew()
            .expect_err("stale owner cannot renew")
            .failure(),
        telchar::service::singleton_ownership::SingletonOwnershipFailure::Fenced
    );
}

#[test]
fn ownership_remains_authoritative_during_long_startup_work() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let mut owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_millis(500),
    )
    .expect("daemon acquires ownership");

    owner
        .maintain_during(Duration::from_millis(100), || {
            std::thread::sleep(Duration::from_millis(900));
            assert_eq!(
                telchar::service::singleton_ownership::SingletonOwnership::acquire(
                    fixture.url(),
                    Duration::from_millis(500),
                )
                .expect_err("startup owner remains authoritative")
                .failure(),
                telchar::service::singleton_ownership::SingletonOwnershipFailure::Contended
            );
        })
        .expect("ownership renewal remains active");

    owner.verify().expect("startup owner remains current");
}

#[test]
fn ownership_release_is_bounded_when_its_row_is_locked() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("daemon acquires ownership");
    let generation = owner.generation();
    let mut observer = fixture.connect();
    let mut transaction = observer.transaction().expect("observer transaction");
    transaction
        .query_one(
            "SELECT generation FROM singleton_ownership WHERE owner_kind = 'daemon' FOR SHARE",
            &[],
        )
        .expect("hold ownership row lock");

    let started = std::time::Instant::now();
    drop(owner);
    let elapsed = started.elapsed();
    assert_eq!(
        transaction
            .query_one(
                "SELECT generation FROM singleton_ownership WHERE owner_kind = 'daemon'",
                &[]
            )
            .expect("unreleased lease remains")
            .get::<_, i64>(0),
        generation
    );
    transaction.rollback().expect("release observer lock");
    assert!(
        elapsed < Duration::from_secs(1),
        "ownership release exceeded shutdown budget: {elapsed:?}"
    );
}

#[test]
fn ownership_reuses_its_connection_during_renewal() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("daemon acquires ownership");
    let mut observer = fixture.connect();
    observer
        .batch_execute("SELECT pg_stat_clear_snapshot()")
        .expect("statistics snapshot clears");
    let backends = |observer: &mut postgres::Client| {
        observer
            .query(
                "SELECT pid FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid() ORDER BY pid",
                &[],
            )
            .expect("ownership backend identities read")
            .iter()
            .map(|row| row.get::<_, i32>(0))
            .collect::<Vec<_>>()
    };
    let initial_backends = backends(&mut observer);
    assert_eq!(
        initial_backends.len(),
        1,
        "ownership retains one connection"
    );
    for _ in 0..3 {
        owner.renew().expect("ownership renews");
        owner.verify().expect("ownership verifies");
        assert_eq!(backends(&mut observer), initial_backends);
    }
}

#[test]
fn ownership_renews_after_database_restart() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let mut owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("daemon acquires ownership");

    fixture.restart();

    owner
        .check()
        .expect("database-backed lease renews after PostgreSQL recovers");
    assert_eq!(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(20),
        )
        .expect_err("renewed owner remains authoritative")
        .failure(),
        telchar::service::singleton_ownership::SingletonOwnershipFailure::Contended
    );
}

#[test]
fn startup_survives_connection_outage_within_confirmed_lease() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let mut owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(15),
    )
    .unwrap();
    let generation = owner.generation();
    owner
        .maintain_during(Duration::from_millis(100), || {
            fixture.stop();
            std::thread::sleep(Duration::from_secs(6));
            fixture.restart();
        })
        .expect("startup recovers the existing lease after connection failure");
    assert_eq!(owner.generation(), generation);
    owner.verify().unwrap();
}

#[test]
fn saturated_application_pool_does_not_starve_renewal() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = std::sync::Arc::new(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(3),
        )
        .unwrap(),
    );
    let application = owner.database().clone();
    let held = (0..16)
        .map(|_| application.connection().unwrap())
        .collect::<Vec<_>>();
    let renewal = owner.with_renewal(Duration::from_millis(100)).unwrap();
    std::thread::sleep(Duration::from_millis(3_200));
    let available = owner.admission_available();
    drop(held);
    drop(renewal);
    assert!(
        matches!(available, Ok(true)),
        "healthy database ownership was starved by application traffic: {available:?}"
    );
    owner.verify().unwrap();
}

#[test]
fn microvm_read_transport_failure_pauses_even_when_verification_can_succeed() {
    for cleanup in [false, true] {
        let fixture = PostgresFixture::start();
        telchar::persistence::migrate(fixture.url()).unwrap();
        let owner = std::sync::Arc::new(
            telchar::service::singleton_ownership::SingletonOwnership::acquire(
                fixture.url(),
                Duration::from_secs(15),
            )
            .unwrap(),
        );
        let mut observer = fixture.connect();
        let mut lock = observer.transaction().unwrap();
        lock.batch_execute("LOCK TABLE microvm_launches IN ACCESS EXCLUSIVE MODE")
            .unwrap();
        std::thread::scope(|scope| {
            let read = scope.spawn(|| {
                if cleanup {
                    telchar::persistence::pending_microvm_cleanup(
                        owner.database(),
                        "000000000001",
                        "us-west-2",
                        1,
                        None,
                    )
                    .map(|_| ())
                } else {
                    telchar::persistence::microvm_session_request(owner.database(), "fixture")
                        .map(|_| ())
                }
            });
            let mut killer = fixture.connect();
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let pid = loop {
                let row = killer.query_opt("SELECT pid FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE '%FROM microvm_launches%' AND pid <> pg_backend_pid()", &[]).unwrap();
                if let Some(row) = row {
                    break row.get::<_, i32>(0);
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "MicroVM read did not reach database"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            killer
                .execute("SELECT pg_terminate_backend($1)", &[&pid])
                .unwrap();
            assert!(read.join().unwrap().is_err());
        });
        lock.rollback().unwrap();
        owner.verify().unwrap();
        assert!(
            !owner.admission_available().unwrap(),
            "actual MicroVM transport failure was erased by healthy verification; cleanup={cleanup}"
        );
        owner.renew().unwrap();
        assert!(owner.admission_available().unwrap());
    }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "ownership state did not change"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn renewal_worker_pauses_admission_and_recovers_same_generation_for_both_owners() {
    for local_executor in [false, true] {
        let mut fixture = PostgresFixture::start();
        telchar::persistence::migrate(fixture.url()).unwrap();
        let owner = std::sync::Arc::new(
            if local_executor {
                telchar::service::singleton_ownership::SingletonOwnership::acquire_local_executor(
                    fixture.url(),
                    Duration::from_secs(10),
                )
            } else {
                telchar::service::singleton_ownership::SingletonOwnership::acquire(
                    fixture.url(),
                    Duration::from_secs(10),
                )
            }
            .unwrap(),
        );
        let generation = owner.generation();
        let renewal = owner.with_renewal(Duration::from_millis(100)).unwrap();
        fixture.stop();
        wait_until(|| matches!(owner.admission_available(), Ok(false)));
        let gated = owner.database().clone().with_ownership(&owner);
        let waiting = std::thread::spawn(move || gated.wait_for_ownership());
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !waiting.is_finished(),
            "admission proceeded without database authority"
        );
        fixture.restart();
        wait_until(|| matches!(owner.admission_available(), Ok(true)));
        waiting.join().unwrap().unwrap();
        assert_eq!(owner.generation(), generation);
        owner.verify().unwrap();
        drop(renewal);
    }
}

#[test]
fn outage_through_expiry_never_revives_owner_or_reacquires_after_takeover() {
    for local_executor in [false, true] {
        let mut fixture = PostgresFixture::start();
        telchar::persistence::migrate(fixture.url()).unwrap();
        let acquire = |url: &str| {
            if local_executor {
                telchar::service::singleton_ownership::SingletonOwnership::acquire_local_executor(
                    url,
                    Duration::from_secs(2),
                )
            } else {
                telchar::service::singleton_ownership::SingletonOwnership::acquire(
                    url,
                    Duration::from_secs(2),
                )
            }
        };
        let owner = std::sync::Arc::new(acquire(fixture.url()).unwrap());
        let renewal = owner.with_renewal(Duration::from_millis(100)).unwrap();
        fixture.stop();
        wait_until(|| owner.admission_available().is_err());
        assert!(owner.wait_for_authority().is_err());
        fixture.restart();
        let replacement = acquire(fixture.url()).unwrap();
        assert!(replacement.generation() > owner.generation());
        assert_eq!(
            owner.renew().unwrap_err().failure(),
            telchar::service::singleton_ownership::SingletonOwnershipFailure::Fenced
        );
        assert!(owner.admission_available().is_err());
        assert!(
            telchar::persistence::create_build_request(
                owner.database(),
                "expired-write",
                "/nix/store/00000000000000000000000000000000-expired.drv",
                "x86_64-linux",
                "fixture",
                "fixture",
            )
            .is_err()
        );
        drop(renewal);
        drop(owner);
        replacement.verify().unwrap();
    }
}

#[test]
fn nontransient_renewal_query_error_is_terminal_not_recovery() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(10),
    )
    .unwrap();
    fixture
        .connect()
        .batch_execute("ALTER TABLE singleton_ownership RENAME TO unavailable_ownership")
        .unwrap();
    assert_eq!(
        owner.renew().unwrap_err().failure(),
        telchar::service::singleton_ownership::SingletonOwnershipFailure::Query
    );
    assert_eq!(
        owner.admission_available().unwrap_err().failure(),
        telchar::service::singleton_ownership::SingletonOwnershipFailure::Query
    );
    fixture
        .connect()
        .batch_execute("ALTER TABLE unavailable_ownership RENAME TO singleton_ownership")
        .unwrap();
    assert!(
        owner.renew().is_err(),
        "terminal owner revived after schema repair"
    );
}

#[test]
fn verification_cannot_restore_admission_without_confirmed_renewal() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(10),
    )
    .unwrap();
    fixture.stop();
    assert!(!owner.verify_available().unwrap());
    fixture.restart();
    owner.verify().unwrap();
    assert!(
        !owner.admission_available().unwrap(),
        "read-only observation reopened admission"
    );
    owner.renew().unwrap();
    assert!(owner.admission_available().unwrap());
}

#[test]
fn unconfirmed_renewal_does_not_extend_last_confirmed_stop_bound() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = std::sync::Arc::new(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(2),
        )
        .unwrap(),
    );
    let mut observer = fixture.connect();
    let mut locked = observer.transaction().unwrap();
    locked
        .query_one(
            "SELECT generation FROM singleton_ownership WHERE owner_kind = 'daemon' FOR SHARE",
            &[],
        )
        .unwrap();
    let started = std::time::Instant::now();
    let renewal = owner.with_renewal(Duration::from_millis(100)).unwrap();
    wait_until(|| matches!(owner.admission_available(), Ok(false)));
    wait_until(|| owner.admission_available().is_err());
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "timed-out queries extended stop bound"
    );
    locked.rollback().unwrap();
    assert!(owner.renew().is_err());
    drop(renewal);
}
