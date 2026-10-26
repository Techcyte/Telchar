mod support;

use support::postgres::PostgresFixture;

#[derive(Clone)]
struct TestDatabase {
    database: telchar::persistence::Database,
}

impl telchar::persistence::DatabaseSource for TestDatabase {
    fn database(
        &self,
    ) -> Result<telchar::persistence::Database, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.database.clone())
    }
}

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

#[test]
fn database_opens_connections_only_for_concurrent_checkouts() {
    let fixture = PostgresFixture::start();
    let mut observer = fixture.connect();
    let database = telchar::persistence::Database::connect(fixture.url())
        .expect("database connection pool starts");
    let mut first = database.connection().expect("first connection checks out");
    let first_backend: i32 = first
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("first backend identity reads")
        .get(0);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let connections: i64 = observer
        .query_one(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid()",
            &[],
        )
        .expect("database connection count reads")
        .get(0);
    assert_eq!(connections, 1, "one checkout needs one database connection");

    let mut second = database
        .connection()
        .expect("concurrent connection checks out");
    let second_backend: i32 = second
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("second backend identity reads")
        .get(0);
    assert_ne!(first_backend, second_backend);
    let connections: i64 = observer
        .query_one(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid()",
            &[],
        )
        .expect("concurrent database connection count reads")
        .get(0);
    assert_eq!(connections, 2);
    drop(second);
    drop(first);
    let reused_backend: i32 = database
        .connection()
        .expect("returned connection checks out")
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("reused backend identity reads")
        .get(0);
    assert_eq!(first_backend, reused_backend);
}

#[test]
fn persistence_accepts_a_substituted_database_source() {
    let fixture = PostgresFixture::start();
    let database = TestDatabase {
        database: telchar::persistence::Database::connect(fixture.url())
            .expect("test database configures"),
    };

    let migration = telchar::persistence::migrate(&database).expect("migration succeeds");

    assert_eq!(
        migration.resulting_version,
        telchar::persistence::latest_migration_version()
    );
}
