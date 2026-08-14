//! Tests singleton ownership contracts and failure boundaries, including only one owner acquires the database lifetime lock.

mod support;

use support::postgres::PostgresFixture;

#[test]
fn only_one_owner_acquires_the_database_lifetime_lock() {
    let fixture = PostgresFixture::start();

    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(fixture.url())
        .expect("first daemon acquires ownership");
    assert_eq!(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(fixture.url())
            .expect_err("second daemon is refused")
            .failure(),
        telchar::service::singleton_ownership::SingletonOwnershipFailure::Contended
    );

    drop(owner);
    telchar::service::singleton_ownership::SingletonOwnership::acquire(fixture.url())
        .expect("replacement acquires after authoritative release");
}

#[test]
fn ownership_check_fails_after_database_connection_loss() {
    let mut fixture = PostgresFixture::start();
    let mut owner =
        telchar::service::singleton_ownership::SingletonOwnership::acquire(fixture.url())
            .expect("daemon acquires ownership");

    fixture.restart();

    assert_eq!(
        owner
            .check()
            .expect_err("dead lifetime connection fences owner")
            .failure(),
        telchar::service::singleton_ownership::SingletonOwnershipFailure::Connection
    );
    telchar::service::singleton_ownership::SingletonOwnership::acquire(fixture.url())
        .expect("replacement acquires after PostgreSQL releases dead session");
}
