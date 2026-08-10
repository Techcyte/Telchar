use std::fmt;

use postgres::{Client, NoTls};

pub const SINGLETON_OWNERSHIP_LOCK_KEY: i64 = 0x5445_4c43_4841_5202;
pub const LOCAL_EXECUTOR_OWNERSHIP_LOCK_KEY: i64 = 0x5445_4c43_4841_5203;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SingletonOwnershipFailure {
    Configuration,
    Connection,
    Query,
    Contended,
}

impl SingletonOwnershipFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Connection => "connection",
            Self::Query => "query",
            Self::Contended => "contended",
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
    connection: Client,
}

impl fmt::Debug for SingletonOwnership {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SingletonOwnership")
    }
}

impl SingletonOwnership {
    pub fn acquire(database_url: &str) -> Result<Self, SingletonOwnershipError> {
        Self::acquire_with_key(database_url, SINGLETON_OWNERSHIP_LOCK_KEY)
    }

    pub fn acquire_local_executor(database_url: &str) -> Result<Self, SingletonOwnershipError> {
        Self::acquire_with_key(database_url, LOCAL_EXECUTOR_OWNERSHIP_LOCK_KEY)
    }

    fn acquire_with_key(
        database_url: &str,
        lock_key: i64,
    ) -> Result<Self, SingletonOwnershipError> {
        if database_url.trim().is_empty() {
            return Err(SingletonOwnershipError(
                SingletonOwnershipFailure::Configuration,
            ));
        }
        let mut connection = Client::connect(database_url, NoTls)
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Connection))?;
        let acquired: bool = connection
            .query_one("SELECT pg_try_advisory_lock($1)", &[&lock_key])
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Query))?
            .try_get(0)
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Query))?;
        if !acquired {
            return Err(SingletonOwnershipError(
                SingletonOwnershipFailure::Contended,
            ));
        }
        Ok(Self { connection })
    }

    pub fn check(&mut self) -> Result<(), SingletonOwnershipError> {
        let value: i32 = self
            .connection
            .query_one("SELECT 1", &[])
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Connection))?
            .try_get(0)
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Query))?;
        if value != 1 {
            return Err(SingletonOwnershipError(SingletonOwnershipFailure::Query));
        }
        Ok(())
    }
}
