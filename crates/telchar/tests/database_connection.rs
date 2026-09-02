mod support;

use support::postgres::PostgresFixture;

#[test]
fn database_reuses_an_established_connection() {
    let fixture = PostgresFixture::start();
    let database = telchar::persistence::Database::connect(fixture.url())
        .expect("database connection pool starts");

    let first_backend = database
        .connection()
        .expect("first database connection checks out")
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("first backend identity reads")
        .get::<_, i32>(0);
    let second_backend = database
        .connection()
        .expect("second database connection checks out")
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("second backend identity reads")
        .get::<_, i32>(0);

    assert_eq!(first_backend, second_backend);
}
