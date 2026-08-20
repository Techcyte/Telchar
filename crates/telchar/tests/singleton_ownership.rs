//! Tests singleton ownership contracts and failure boundaries, including only one owner acquires the database lifetime lock.

mod support;

use std::time::Duration;

use support::postgres::PostgresFixture;

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
}

#[test]
fn renewal_extends_lease_only_for_current_owner() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let mut owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
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
