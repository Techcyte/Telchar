mod support;

use std::time::{Duration, SystemTime};

use postgres::{Client, NoTls};
use support::postgres::PostgresFixture;
use telchar::persistence::reserve_nomad_callback_nonce;

#[test]
fn reserves_callback_nonce_once_and_survives_new_connections() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let expiry = SystemTime::now() + Duration::from_secs(600);

    assert!(
        reserve_nomad_callback_nonce(
            fixture.url(),
            "nomad-primary",
            "job-1",
            "allocation-1",
            "request-nonce-1",
            expiry,
            8,
        )
        .expect("first nonce reserves")
    );
    assert!(
        !reserve_nomad_callback_nonce(
            fixture.url(),
            "nomad-primary",
            "job-1",
            "allocation-1",
            "request-nonce-1",
            expiry,
            8,
        )
        .expect("replayed nonce rejects")
    );

    let mut client = Client::connect(fixture.url(), NoTls).expect("PostgreSQL connects");
    let row = client
        .query_one(
            "SELECT octet_length(nonce_digest), backend_name, job_id, allocation_id FROM nomad_callback_nonces",
            &[],
        )
        .expect("nonce row queries");
    assert_eq!(row.get::<_, i32>(0), 32);
    assert_eq!(row.get::<_, String>(1), "nomad-primary");
    assert_eq!(row.get::<_, String>(2), "job-1");
    assert_eq!(row.get::<_, String>(3), "allocation-1");
}

#[test]
fn purges_expired_nonces_and_fails_closed_at_capacity() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let future = SystemTime::now() + Duration::from_secs(600);

    assert!(
        reserve_nomad_callback_nonce(
            fixture.url(),
            "nomad-primary",
            "job-1",
            "allocation-1",
            "request-nonce-1",
            future,
            1,
        )
        .expect("first nonce reserves")
    );
    assert!(
        reserve_nomad_callback_nonce(
            fixture.url(),
            "nomad-primary",
            "job-1",
            "allocation-1",
            "request-nonce-2",
            future,
            1,
        )
        .is_err()
    );

    let mut client = Client::connect(fixture.url(), NoTls).expect("PostgreSQL connects");
    client
        .execute(
            "UPDATE nomad_callback_nonces SET created_at = transaction_timestamp() - interval '2 seconds', expires_at = transaction_timestamp() - interval '1 second'",
            &[],
        )
        .expect("nonce expires");
    assert!(
        reserve_nomad_callback_nonce(
            fixture.url(),
            "nomad-primary",
            "job-1",
            "allocation-1",
            "request-nonce-2",
            future,
            1,
        )
        .expect("expired nonce capacity is reclaimed")
    );
}
