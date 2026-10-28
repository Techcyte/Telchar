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
