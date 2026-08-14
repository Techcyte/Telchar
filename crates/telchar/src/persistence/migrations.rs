use super::*;

const MIGRATION_LEDGER_SQL: &str = "\
CREATE TABLE IF NOT EXISTS telchar_schema_migrations (\
    version bigint PRIMARY KEY,\
    name text NOT NULL UNIQUE,\
    checksum bytea NOT NULL CHECK (octet_length(checksum) = 32),\
    applied_at timestamptz NOT NULL DEFAULT now()\
)";

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "minimum_lifecycle",
        sql: include_str!("../../migrations/0001_minimum_lifecycle.sql"),
    },
    Migration {
        version: 2,
        name: "output_retention",
        sql: include_str!("../../migrations/0002_output_retention.sql"),
    },
    Migration {
        version: 3,
        name: "execution_state",
        sql: include_str!("../../migrations/0003_execution_state.sql"),
    },
    Migration {
        version: 4,
        name: "reconciliation_state",
        sql: include_str!("../../migrations/0004_reconciliation_state.sql"),
    },
    Migration {
        version: 5,
        name: "local_backend_registry",
        sql: include_str!("../../migrations/0005_local_backend_registry.sql"),
    },
    Migration {
        version: 6,
        name: "local_backend_results",
        sql: include_str!("../../migrations/0006_local_backend_results.sql"),
    },
    Migration {
        version: 7,
        name: "protocol_session_credentials",
        sql: include_str!("../../migrations/0007_protocol_session_credentials.sql"),
    },
    Migration {
        version: 8,
        name: "retained_store_paths",
        sql: include_str!("../../migrations/0008_retained_store_paths.sql"),
    },
    Migration {
        version: 9,
        name: "shared_builds",
        sql: include_str!("../../migrations/0009_shared_builds.sql"),
    },
    Migration {
        version: 10,
        name: "shared_build_scheduling",
        sql: include_str!("../../migrations/0010_shared_build_scheduling.sql"),
    },
    Migration {
        version: 11,
        name: "shared_build_scheduler",
        sql: include_str!("../../migrations/0011_shared_build_scheduler.sql"),
    },
    Migration {
        version: 12,
        name: "shared_build_attempts",
        sql: include_str!("../../migrations/0012_shared_build_attempts.sql"),
    },
    Migration {
        version: 13,
        name: "shared_build_authority",
        sql: include_str!("../../migrations/0013_shared_build_authority.sql"),
    },
    Migration {
        version: 14,
        name: "nomad_callback_nonces",
        sql: include_str!("../../migrations/0014_nomad_callback_nonces.sql"),
    },
    Migration {
        version: 15,
        name: "shared_build_specification",
        sql: include_str!("../../migrations/0015_shared_build_specification.sql"),
    },
];

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MigrationFailure {
    Configuration,
    Connection,
    Lock,
    Ledger,
    Checksum,
    FutureVersion,
    MigrationSql,
    Commit,
}

impl MigrationFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Connection => "connection",
            Self::Lock => "lock",
            Self::Ledger => "ledger",
            Self::Checksum => "checksum",
            Self::FutureVersion => "future-version",
            Self::MigrationSql => "migration-sql",
            Self::Commit => "commit",
        }
    }
}

#[derive(Debug)]
pub struct MigrationError(MigrationFailure);

impl MigrationError {
    pub fn failure(&self) -> MigrationFailure {
        self.0
    }
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("database migration failed")
    }
}

impl std::error::Error for MigrationError {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct MigrationOutcome {
    pub previously_applied: usize,
    pub applied_this_run: usize,
    pub resulting_version: i64,
}

pub fn latest_migration_version() -> i64 {
    MIGRATIONS.last().map_or(0, |migration| migration.version)
}

pub fn migrate(database_url: &str) -> Result<MigrationOutcome, MigrationError> {
    migrate_list(database_url, MIGRATIONS)
}

fn migrate_list(
    database_url: &str,
    migrations: &[Migration],
) -> Result<MigrationOutcome, MigrationError> {
    if database_url.trim().is_empty() {
        return Err(MigrationError(MigrationFailure::Configuration));
    }
    validate_migrations(migrations).map_err(MigrationError)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| MigrationError(MigrationFailure::Connection))?;
    run_migrations(&mut client, migrations).map_err(MigrationError)
}

fn validate_migrations(migrations: &[Migration]) -> Result<(), MigrationFailure> {
    let mut prior = 0;
    let mut names = std::collections::HashSet::new();
    for migration in migrations {
        if migration.version <= prior || migration.name.is_empty() || !names.insert(migration.name)
        {
            return Err(MigrationFailure::Configuration);
        }
        prior = migration.version;
    }
    Ok(())
}

fn run_migrations(
    client: &mut Client,
    migrations: &[Migration],
) -> Result<MigrationOutcome, MigrationFailure> {
    let mut transaction = client
        .transaction()
        .map_err(|_| MigrationFailure::Connection)?;
    transaction
        .query_one("SELECT pg_advisory_xact_lock($1)", &[&MIGRATION_LOCK_KEY])
        .map_err(|_| MigrationFailure::Lock)?;
    transaction
        .batch_execute(MIGRATION_LEDGER_SQL)
        .map_err(|_| MigrationFailure::Ledger)?;
    let rows = transaction
        .query(
            "SELECT version, name, checksum FROM telchar_schema_migrations ORDER BY version",
            &[],
        )
        .map_err(|_| MigrationFailure::Ledger)?;

    for (index, row) in rows.iter().enumerate() {
        let migration = migrations
            .get(index)
            .ok_or(MigrationFailure::FutureVersion)?;
        let version: i64 = row.get(0);
        let name: String = row.get(1);
        let applied_checksum: Vec<u8> = row.get(2);
        if version != migration.version || name != migration.name {
            return Err(MigrationFailure::Ledger);
        }
        if applied_checksum != checksum(migration.sql) {
            return Err(MigrationFailure::Checksum);
        }
    }

    let previously_applied = rows.len();
    for migration in &migrations[previously_applied..] {
        transaction
            .batch_execute(migration.sql)
            .map_err(|_| MigrationFailure::MigrationSql)?;
        transaction
            .execute(
                "INSERT INTO telchar_schema_migrations (version, name, checksum) VALUES ($1, $2, $3)",
                &[&migration.version, &migration.name, &checksum(migration.sql)],
            )
            .map_err(|_| MigrationFailure::Ledger)?;
    }
    transaction.commit().map_err(|_| MigrationFailure::Commit)?;
    Ok(MigrationOutcome {
        previously_applied,
        applied_this_run: migrations.len() - previously_applied,
        resulting_version: migrations.last().map_or(0, |migration| migration.version),
    })
}

fn checksum(sql: &str) -> Vec<u8> {
    Sha256::digest(sql.as_bytes()).to_vec()
}

#[cfg(test)]
mod tests {
    mod postgres {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/postgres.rs"
        ));
    }

    use super::*;
    use postgres::PostgresFixture;

    #[test]
    fn migration_metadata_is_valid() {
        assert!(validate_migrations(MIGRATIONS).is_ok());
    }

    #[test]
    fn failed_pending_migration_rolls_back_schema_and_ledger() {
        let fixture = PostgresFixture::start();
        let migrations = [
            Migration {
                version: 1,
                name: "first",
                sql: "CREATE TABLE migration_rollback_proof (value integer)",
            },
            Migration {
                version: 2,
                name: "fails",
                sql: "CREATE TABLE migration_rollback_proof (value integer)",
            },
        ];

        let error = migrate_list(fixture.url(), &migrations).expect_err("later migration fails");

        assert_eq!(error.failure(), MigrationFailure::MigrationSql);
        let mut client = Client::connect(fixture.url(), NoTls).expect("test database reconnects");
        assert!(client
            .query_one("SELECT to_regclass('migration_rollback_proof')::text", &[])
            .expect("table lookup succeeds")
            .get::<_, Option<String>>(0)
            .is_none());
        assert!(client
            .query_one("SELECT to_regclass('telchar_schema_migrations')::text", &[])
            .expect("ledger lookup succeeds")
            .get::<_, Option<String>>(0)
            .is_none());
    }

    #[test]
    fn invalid_migration_metadata_fails_closed() {
        for migrations in [
            vec![
                Migration {
                    version: 1,
                    name: "one",
                    sql: "SELECT 1",
                },
                Migration {
                    version: 1,
                    name: "two",
                    sql: "SELECT 2",
                },
            ],
            vec![Migration {
                version: 1,
                name: "",
                sql: "SELECT 1",
            }],
            vec![
                Migration {
                    version: 2,
                    name: "two",
                    sql: "SELECT 2",
                },
                Migration {
                    version: 1,
                    name: "one",
                    sql: "SELECT 1",
                },
            ],
            vec![
                Migration {
                    version: 1,
                    name: "same",
                    sql: "SELECT 1",
                },
                Migration {
                    version: 2,
                    name: "same",
                    sql: "SELECT 2",
                },
            ],
        ] {
            assert_eq!(
                validate_migrations(&migrations),
                Err(MigrationFailure::Configuration)
            );
        }
    }
}
