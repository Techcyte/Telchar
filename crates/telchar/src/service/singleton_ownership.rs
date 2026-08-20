//! Acquires and renews the PostgreSQL lease that fences Telchar to one active process.

use std::fmt;
use std::time::Duration;

use postgres::{Client, Config, NoTls};

const DAEMON_OWNER_KIND: &str = "daemon";
const LOCAL_EXECUTOR_OWNER_KIND: &str = "local-executor";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SingletonOwnershipFailure {
    Configuration,
    Connection,
    Query,
    Contended,
    Fenced,
}

impl SingletonOwnershipFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Connection => "connection",
            Self::Query => "query",
            Self::Contended => "contended",
            Self::Fenced => "fenced",
        }
    }
}

#[derive(Debug)]
pub struct SingletonOwnershipError(SingletonOwnershipFailure);

impl SingletonOwnershipError {
    pub fn failure(&self) -> SingletonOwnershipFailure {
        self.0
    }
}

impl fmt::Display for SingletonOwnershipError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("singleton ownership operation failed")
    }
}

impl std::error::Error for SingletonOwnershipError {}

pub struct SingletonOwnership {
    database_url: String,
    owner_kind: &'static str,
    owner_token: String,
    generation: i64,
    lease_duration: Duration,
}

impl fmt::Debug for SingletonOwnership {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SingletonOwnership")
    }
}

impl SingletonOwnership {
    pub fn acquire(
        database_url: &str,
        lease_duration: Duration,
    ) -> Result<Self, SingletonOwnershipError> {
        Self::acquire_with_kind(database_url, DAEMON_OWNER_KIND, lease_duration)
    }

    pub fn acquire_local_executor(
        database_url: &str,
        lease_duration: Duration,
    ) -> Result<Self, SingletonOwnershipError> {
        Self::acquire_with_kind(database_url, LOCAL_EXECUTOR_OWNER_KIND, lease_duration)
    }

    fn acquire_with_kind(
        database_url: &str,
        owner_kind: &'static str,
        lease_duration: Duration,
    ) -> Result<Self, SingletonOwnershipError> {
        let lease_milliseconds = lease_milliseconds(database_url, lease_duration)?;
        let owner_token = owner_token();
        let mut connection = Client::connect(database_url, NoTls)
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Connection))?;
        let row = connection
            .query_opt(
                "INSERT INTO singleton_ownership (owner_kind, owner_token, generation, lease_expires_at, updated_at) VALUES ($1, $2, 1, clock_timestamp() + ($3::bigint * interval '1 millisecond'), clock_timestamp()) ON CONFLICT (owner_kind) DO UPDATE SET owner_token = EXCLUDED.owner_token, generation = singleton_ownership.generation + 1, lease_expires_at = EXCLUDED.lease_expires_at, updated_at = EXCLUDED.updated_at WHERE singleton_ownership.lease_expires_at <= clock_timestamp() RETURNING generation",
                &[&owner_kind, &owner_token, &lease_milliseconds],
            )
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Query))?;
        let Some(row) = row else {
            return Err(SingletonOwnershipError(
                SingletonOwnershipFailure::Contended,
            ));
        };
        let generation = row
            .try_get(0)
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Query))?;
        Ok(Self {
            database_url: fenced_database_url(database_url, owner_kind, &owner_token, generation)?,
            owner_kind,
            owner_token,
            generation,
            lease_duration,
        })
    }

    pub fn generation(&self) -> i64 {
        self.generation
    }

    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    pub fn renew(&mut self) -> Result<(), SingletonOwnershipError> {
        let lease_milliseconds = lease_milliseconds(&self.database_url, self.lease_duration)?;
        let mut connection = Client::connect(&self.database_url, NoTls)
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Connection))?;
        let renewed = connection
            .execute(
                "UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() + ($4::bigint * interval '1 millisecond'), updated_at = clock_timestamp() WHERE owner_kind = $1 AND owner_token = $2 AND generation = $3 AND lease_expires_at > clock_timestamp()",
                &[&self.owner_kind, &self.owner_token, &self.generation, &lease_milliseconds],
            )
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Query))?;
        if renewed != 1 {
            return Err(SingletonOwnershipError(SingletonOwnershipFailure::Fenced));
        }
        Ok(())
    }

    pub fn check(&mut self) -> Result<(), SingletonOwnershipError> {
        self.renew()
    }

    pub fn verify(&self) -> Result<(), SingletonOwnershipError> {
        let mut connection = Client::connect(&self.database_url, NoTls)
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Connection))?;
        let current = connection
            .query_opt(
                "SELECT 1 FROM singleton_ownership WHERE owner_kind = $1 AND owner_token = $2 AND generation = $3 AND lease_expires_at > clock_timestamp()",
                &[&self.owner_kind, &self.owner_token, &self.generation],
            )
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Query))?;
        if current.is_none() {
            return Err(SingletonOwnershipError(SingletonOwnershipFailure::Fenced));
        }
        Ok(())
    }
}

impl Drop for SingletonOwnership {
    fn drop(&mut self) {
        let Ok(mut connection) = Client::connect(&self.database_url, NoTls) else {
            return;
        };
        let _ = connection.execute(
            "DELETE FROM singleton_ownership WHERE owner_kind = $1 AND owner_token = $2 AND generation = $3",
            &[&self.owner_kind, &self.owner_token, &self.generation],
        );
    }
}

fn lease_milliseconds(
    database_url: &str,
    lease_duration: Duration,
) -> Result<i64, SingletonOwnershipError> {
    if database_url.trim().is_empty() || lease_duration.is_zero() {
        return Err(SingletonOwnershipError(
            SingletonOwnershipFailure::Configuration,
        ));
    }
    i64::try_from(lease_duration.as_millis())
        .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Configuration))
}

fn fenced_database_url(
    database_url: &str,
    owner_kind: &str,
    owner_token: &str,
    generation: i64,
) -> Result<String, SingletonOwnershipError> {
    let _: Config = database_url
        .parse()
        .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Configuration))?;
    let options = format!(
        "-c telchar.owner_kind={owner_kind} -c telchar.owner_token={owner_token} -c telchar.owner_generation={generation}"
    );
    if database_url.starts_with("postgres://") || database_url.starts_with("postgresql://") {
        Ok(format!(
            "{database_url}{}options={}",
            if database_url.contains('?') { "&" } else { "?" },
            options.replace(' ', "%20").replace('=', "%3D")
        ))
    } else {
        Ok(format!("{database_url} options='{options}'"))
    }
}

fn owner_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        time,
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}
