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
