//! Acquires and renews the PostgreSQL lease that fences Telchar to one active process.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::persistence::{connect, validate_connection};

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
    database: crate::persistence::Database,
    owner_kind: &'static str,
    owner_token: String,
    generation: i64,
    lease_duration: Duration,
    health: Mutex<OwnershipHealth>,
    renewal: Mutex<()>,
}

struct OwnershipHealth {
    // Starts before the confirmed SQL operation, never at response receipt.
    deadline: Instant,
    recovering: bool,
    failure: Option<SingletonOwnershipFailure>,
}

pub struct OwnershipRenewal {
    stop: Option<std::sync::mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for OwnershipRenewal {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
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
        let lease_duration = Duration::from_millis(lease_milliseconds as u64);
        let owner_token = owner_token();
        let mut connection = connect(database_url)
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Connection))?;
        let confirmed_start = Instant::now();
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
        drop(connection);
        let database_url = fenced_database_url(database_url, owner_kind, &owner_token, generation)?;
        let database = crate::persistence::Database::connect(&database_url)
            .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Connection))?;
        let ownership = Self {
            database_url,
            database,
            owner_kind,
            owner_token,
            generation,
            lease_duration,
            health: Mutex::new(OwnershipHealth {
                deadline: confirmed_start + lease_duration,
                recovering: false,
                failure: None,
            }),
            renewal: Mutex::new(()),
        };
        ownership.verify()?;
        Ok(ownership)
    }

    pub fn generation(&self) -> i64 {
        self.generation
    }

    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    pub fn database(&self) -> &crate::persistence::Database {
        &self.database
    }

    /// Renew only the existing generation, within its last confirmed conservative window.
    pub fn renew(&self) -> Result<(), SingletonOwnershipError> {
        let _renewal = self.renewal.lock().expect("ownership renewal mutex");
        let deadline = {
            let mut health = self.health.lock().expect("ownership health mutex");
            health.check()?;
            health.deadline
        };
        let started = Instant::now();
        let result = renew_lease(
            &self.database,
            self.owner_kind,
            &self.owner_token,
            self.generation,
            self.lease_duration,
            deadline.min(started + Duration::from_secs(1)),
        );
        let mut health = self.health.lock().expect("ownership health mutex");
        // Even an ambiguous successful write must not revive an owner after its stop bound.
        health.check()?;
        match result {
            Ok(()) => {
                health.confirm_renewal(started, self.lease_duration, self.owner_kind);
                Ok(())
            }
            Err(error) => {
                health.record_failure(error.failure(), self.owner_kind);
                Err(error)
            }
        }
    }

    /// False pauses new admission and dispatch; it does not cancel running work.
    pub fn admission_available(&self) -> Result<bool, SingletonOwnershipError> {
        let mut health = self.health.lock().expect("ownership health mutex");
        health.check()?;
        Ok(!health.recovering)
    }

    pub fn wait_for_authority(&self) -> Result<(), SingletonOwnershipError> {
        while !self.admission_available()? {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    pub fn with_renewal(
        self: &Arc<Self>,
        interval: Duration,
    ) -> Result<OwnershipRenewal, SingletonOwnershipError> {
        if interval.is_zero() || interval >= self.lease_duration {
            return Err(SingletonOwnershipError(
                SingletonOwnershipFailure::Configuration,
            ));
        }
        let owner = Arc::clone(self);
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || owner.renew_until_stopped(interval, &stopped));
        Ok(OwnershipRenewal {
            stop: Some(stop),
            worker: Some(worker),
        })
    }

    fn renew_until_stopped(&self, interval: Duration, stopped: &std::sync::mpsc::Receiver<()>) {
        let mut delay = interval;
        let mut backoff = Duration::from_millis(50);
        loop {
            let remaining = {
                let mut health = self.health.lock().expect("ownership health mutex");
                if health.check().is_err() {
                    return;
                }
                health.deadline.saturating_duration_since(Instant::now())
            };
            match stopped.recv_timeout(delay.min(remaining)) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            match self.renew() {
                Ok(()) => {
                    delay = interval;
                    backoff = Duration::from_millis(50);
                }
                Err(error) if error.failure() == SingletonOwnershipFailure::Connection => {
                    delay = backoff;
                    backoff = (backoff * 2).min(Duration::from_secs(1));
                }
                Err(_) => return,
            }
        }
    }

    pub fn maintain_during<T>(
        &mut self,
        renewal_interval: Duration,
        operation: impl FnOnce() -> T,
    ) -> Result<T, SingletonOwnershipError> {
        if renewal_interval.is_zero() || renewal_interval >= self.lease_duration {
            return Err(SingletonOwnershipError(
                SingletonOwnershipFailure::Configuration,
            ));
        }
        let (stop, stopped) = std::sync::mpsc::channel();
        let owner = &*self;
        std::thread::scope(|scope| {
            let renewal =
                scope.spawn(move || owner.renew_until_stopped(renewal_interval, &stopped));
            let result = operation();
            // A completed startup operation is not permission to publish readiness during recovery.
            let authority = owner.wait_for_authority();
            drop(stop);
            renewal
                .join()
                .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Connection))?;
            authority?;
            Ok(result)
        })
    }

    pub fn check(&mut self) -> Result<(), SingletonOwnershipError> {
        self.renew()
    }

    pub fn verify(&self) -> Result<(), SingletonOwnershipError> {
        self.verify_database(&self.database)
    }

    /// Observers may pause on transport uncertainty, but cannot restore authority.
    pub fn verify_available(&self) -> Result<bool, SingletonOwnershipError> {
        if !self.admission_available()? {
            return Ok(false);
        }
        match self.verify() {
            Ok(()) => self.admission_available(),
            Err(error) if error.failure() == SingletonOwnershipFailure::Connection => {
                self.admission_available().map(|_| false)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn verify_database(
        &self,
        database: &crate::persistence::Database,
    ) -> Result<(), SingletonOwnershipError> {
        let deadline = {
            let mut health = self.health.lock().expect("ownership health mutex");
            health.check()?;
            health.deadline.min(Instant::now() + Duration::from_secs(1))
        };
        let mut connection = database
            .connection_until(deadline)
            .map_err(|_| self.observe_failure(SingletonOwnershipFailure::Connection))?;
        let current = connection
            .query_opt(
                "SELECT 1 FROM singleton_ownership WHERE owner_kind = $1 AND owner_token = $2 AND generation = $3 AND lease_expires_at > clock_timestamp()",
                &[&self.owner_kind, &self.owner_token, &self.generation],
            )
            .map_err(|error| self.observe_failure(query_failure(&error)))?;
        if current.is_none() {
            return Err(self.observe_failure(SingletonOwnershipFailure::Fenced));
        }
        self.health.lock().expect("ownership health mutex").check()
    }

    pub(crate) fn observe_database_error(&self, error: &crate::persistence::DatabaseError) {
        match query_failure(error) {
            SingletonOwnershipFailure::Connection => {
                self.observe_failure(SingletonOwnershipFailure::Connection);
            }
            SingletonOwnershipFailure::Fenced => {
                self.observe_failure(SingletonOwnershipFailure::Fenced);
            }
            _ => {}
        }
    }

    pub(crate) fn observe_connection_failure(&self) {
        self.observe_failure(SingletonOwnershipFailure::Connection);
    }

    fn observe_failure(&self, failure: SingletonOwnershipFailure) -> SingletonOwnershipError {
        self.health
            .lock()
            .expect("ownership health mutex")
            .record_failure(failure, self.owner_kind);
        SingletonOwnershipError(failure)
    }
}

impl Drop for SingletonOwnership {
    fn drop(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_millis(250);
        let Ok(mut connection) = self.database.connection_until(deadline) else {
            return;
        };
        let _ = connection.execute(
            "DELETE FROM singleton_ownership WHERE owner_kind = $1 AND owner_token = $2 AND generation = $3",
            &[&self.owner_kind, &self.owner_token, &self.generation],
        );
    }
}

fn renew_lease(
    database: &crate::persistence::Database,
    owner_kind: &str,
    owner_token: &str,
    generation: i64,
    lease_duration: Duration,
    deadline: Instant,
) -> Result<(), SingletonOwnershipError> {
    let lease_milliseconds = i64::try_from(lease_duration.as_millis())
        .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Configuration))?;
    let mut connection = database
        .connection_until(deadline)
        .map_err(|_| SingletonOwnershipError(SingletonOwnershipFailure::Connection))?;
    let renewed = connection
        .execute(
            "UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() + ($4::bigint * interval '1 millisecond'), updated_at = clock_timestamp() WHERE owner_kind = $1 AND owner_token = $2 AND generation = $3 AND lease_expires_at > clock_timestamp()",
            &[&owner_kind, &owner_token, &generation, &lease_milliseconds],
        )
        .map_err(|error| SingletonOwnershipError(query_failure(&error)))?;
    if renewed != 1 {
        return Err(SingletonOwnershipError(SingletonOwnershipFailure::Fenced));
    }
    Ok(())
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
        .ok()
        .filter(|duration| *duration > 0)
        .ok_or(SingletonOwnershipError(
            SingletonOwnershipFailure::Configuration,
        ))
}

fn fenced_database_url(
    database_url: &str,
    owner_kind: &str,
    owner_token: &str,
    generation: i64,
) -> Result<String, SingletonOwnershipError> {
    validate_connection(database_url)
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

impl OwnershipHealth {
    fn confirm_renewal(&mut self, started: Instant, duration: Duration, owner_kind: &str) {
        if self.recovering {
            tracing::info!(event = "database.singleton_ownership.recovered", owner_kind);
        }
        self.deadline = started + duration;
        self.recovering = false;
    }

    fn check(&mut self) -> Result<(), SingletonOwnershipError> {
        if Instant::now() >= self.deadline && self.failure.is_none() {
            self.failure = Some(SingletonOwnershipFailure::Fenced);
        }
        match self.failure {
            Some(failure) => Err(SingletonOwnershipError(failure)),
            None => Ok(()),
        }
    }

    fn record_failure(&mut self, failure: SingletonOwnershipFailure, owner_kind: &str) {
        if failure == SingletonOwnershipFailure::Connection && self.failure.is_none() {
            if !self.recovering {
                tracing::warn!(
                    event = "database.singleton_ownership.recovering",
                    owner_kind,
                    failure_class = failure.as_str()
                );
            }
            self.recovering = true;
        } else if self.failure.is_none() {
            self.failure = Some(failure);
            tracing::error!(
                event = "database.singleton_ownership.lost",
                owner_kind,
                failure_class = failure.as_str()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewal_confirmation_preserves_newer_uncertainty() {
        let started = Instant::now();
        let mut health = OwnershipHealth {
            deadline: started + Duration::from_secs(10),
            recovering: false,
            failure: None,
        };
        // Deterministic order: renewal begins, application fails, renewal response is applied.
        health.record_failure(SingletonOwnershipFailure::Connection, DAEMON_OWNER_KIND);
        health.confirm_renewal(started, Duration::from_secs(10), DAEMON_OWNER_KIND);
        assert!(health.recovering, "older renewal response erased newer uncertainty");
    }
}

fn query_failure(error: &crate::persistence::DatabaseError) -> SingletonOwnershipFailure {
    use crate::persistence::DatabaseError;
    match error {
        DatabaseError::Query(error)
            if error.code().is_some_and(|code| code.code() == "55000")
                && error
                    .as_db_error()
                    .is_some_and(|error| error.message() == "singleton ownership fenced") =>
        {
            SingletonOwnershipFailure::Fenced
        }
        DatabaseError::Closed | DatabaseError::Deadline => SingletonOwnershipFailure::Connection,
        DatabaseError::Query(error) if error.is_closed() || error.as_db_error().is_none() => {
            SingletonOwnershipFailure::Connection
        }
        DatabaseError::Query(error)
            if error.code().is_some_and(|code| {
                code.code().starts_with("08") || matches!(code.code(), "57P01" | "57P02" | "57P03")
            }) =>
        {
            SingletonOwnershipFailure::Connection
        }
        _ => SingletonOwnershipFailure::Query,
    }
}
