use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::time::{Duration, Instant};

use tokio_postgres::{Client, Config, NoTls, Row, SimpleQueryMessage, ToStatement, types::ToSql};
use tokio_postgres_rustls::MakeRustlsConnect;

const CONNECTION_BUDGET: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum DatabaseError {
    Query(tokio_postgres::Error),
    Deadline,
    Cancelled,
    Closed,
    Runtime(std::io::Error),
}

impl DatabaseError {
    pub fn as_db_error(&self) -> Option<&tokio_postgres::error::DbError> {
        match self {
            Self::Query(error) => error.as_db_error(),
            _ => None,
        }
    }

    pub fn code(&self) -> Option<&tokio_postgres::error::SqlState> {
        self.as_db_error().map(|error| error.code())
    }
}

impl std::fmt::Display for DatabaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Query(_) => "database query failed",
            Self::Deadline => {
                "database connection deadline exceeded; mutation outcome may be unknown"
            }
            Self::Cancelled => "database request cancelled; mutation outcome may be unknown",
            Self::Closed => "database connection closed",
            Self::Runtime(_) => "database runtime unavailable",
        })
    }
}

impl std::error::Error for DatabaseError {}

#[derive(Clone)]
pub(super) enum Security {
    Plain,
    Tls(MakeRustlsConnect),
}

pub(super) struct Manager {
    pub config: Config,
    pub security: Security,
}

impl r2d2::ManageConnection for Manager {
    type Connection = DatabaseClient;
    type Error = DatabaseError;

    fn connect(&self) -> Result<DatabaseClient, DatabaseError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(DatabaseError::Runtime)?;
        let deadline = Instant::now() + CONNECTION_BUDGET;
        let (client, connection): (_, ConnectionFuture) = runtime.block_on(async {
            let connect = async {
                match &self.security {
                    Security::Plain => {
                        let (client, connection) = self.config.connect(NoTls).await?;
                        Ok((client, Box::pin(connection) as ConnectionFuture))
                    }
                    Security::Tls(tls) => {
                        let (client, connection) = self.config.connect(tls.clone()).await?;
                        Ok((client, Box::pin(connection) as ConnectionFuture))
                    }
                }
            };
            tokio::time::timeout_at(deadline.into(), connect)
                .await
                .map_err(|_| DatabaseError::Deadline)?
                .map_err(DatabaseError::Query)
        })?;
        Ok(DatabaseClient {
            client,
            driver: Driver {
                runtime,
                connection: Some(connection),
                deadline,
                request: None,
            },
        })
    }

    fn is_valid(&self, connection: &mut DatabaseClient) -> Result<(), DatabaseError> {
        connection.simple_query("").map(|_| ())
    }

    fn has_broken(&self, connection: &mut DatabaseClient) -> bool {
        connection.driver.connection.is_none() || connection.client.is_closed()
    }
}

type ConnectionFuture = Pin<Box<dyn Future<Output = Result<(), tokio_postgres::Error>> + Send>>;

struct Driver {
    runtime: tokio::runtime::Runtime,
    connection: Option<ConnectionFuture>,
    deadline: Instant,
    request: Option<super::RequestBudget>,
}

impl Driver {
    fn run<T>(
        &mut self,
        operation: impl Future<Output = Result<T, tokio_postgres::Error>>,
    ) -> Result<T, DatabaseError> {
        let connection = self.connection.as_mut().ok_or(DatabaseError::Closed)?;
        let mut disconnected = false;
        let request = self.request.clone();
        let result = self.runtime.block_on(async {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(self.deadline.into()) => Err(DatabaseError::Deadline),
                _ = async {
                    if let Some(request) = request {
                        loop {
                            if request.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => Err(DatabaseError::Cancelled),
                result = connection => {
                    disconnected = true;
                    match result {
                        Ok(()) => Err(DatabaseError::Closed),
                        Err(error) => Err(DatabaseError::Query(error)),
                    }
                },
                result = operation => result.map_err(DatabaseError::Query),
            }
        });
        if disconnected
            || matches!(
                result,
                Err(DatabaseError::Deadline | DatabaseError::Cancelled | DatabaseError::Closed)
            )
            || self.client_closed_error(&result)
        {
            self.connection.take();
        }
        result
    }

    fn client_closed_error<T>(&self, result: &Result<T, DatabaseError>) -> bool {
        matches!(result, Err(DatabaseError::Query(error)) if error.is_closed())
    }
}

pub struct DatabaseClient {
    client: Client,
    driver: Driver,
}

impl DatabaseClient {
    pub(super) fn validate(
        &mut self,
        deadline: Instant,
        request: Option<super::RequestBudget>,
    ) -> Result<(), DatabaseError> {
        self.driver.deadline = deadline;
        self.driver.request = request;
        let result = self.simple_query("").map(|_| ());
        if result.is_err() {
            self.driver.connection.take();
        }
        result
    }

    pub fn query<T>(
        &mut self,
        statement: &T,
        parameters: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, DatabaseError>
    where
        T: ?Sized + ToStatement,
    {
        self.driver.run(self.client.query(statement, parameters))
    }

    pub fn query_one<T>(
        &mut self,
        statement: &T,
        parameters: &[&(dyn ToSql + Sync)],
    ) -> Result<Row, DatabaseError>
    where
        T: ?Sized + ToStatement,
    {
        self.driver
            .run(self.client.query_one(statement, parameters))
    }

    pub fn query_opt<T>(
        &mut self,
        statement: &T,
        parameters: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Row>, DatabaseError>
    where
        T: ?Sized + ToStatement,
    {
        self.driver
            .run(self.client.query_opt(statement, parameters))
    }

    pub fn execute<T>(
        &mut self,
        statement: &T,
        parameters: &[&(dyn ToSql + Sync)],
    ) -> Result<u64, DatabaseError>
    where
        T: ?Sized + ToStatement,
    {
        self.driver.run(self.client.execute(statement, parameters))
    }

    pub fn batch_execute(&mut self, query: &str) -> Result<(), DatabaseError> {
        self.driver.run(self.client.batch_execute(query))
    }

    pub fn simple_query(&mut self, query: &str) -> Result<Vec<SimpleQueryMessage>, DatabaseError> {
        self.driver.run(self.client.simple_query(query))
    }

    pub fn transaction(&mut self) -> Result<DatabaseTransaction<'_>, DatabaseError> {
        self.batch_execute("BEGIN")?;
        Ok(DatabaseTransaction {
            client: self,
            done: false,
        })
    }
}

pub struct DatabaseTransaction<'a> {
    client: &'a mut DatabaseClient,
    done: bool,
}

impl DatabaseTransaction<'_> {
    pub fn commit(mut self) -> Result<(), DatabaseError> {
        self.done = true;
        let result = self.client.batch_execute("COMMIT");
        if result.is_err() {
            self.client.driver.connection.take();
        }
        result
    }

    pub fn rollback(mut self) -> Result<(), DatabaseError> {
        self.done = true;
        let result = self.client.batch_execute("ROLLBACK");
        if result.is_err() {
            self.client.driver.connection.take();
        }
        result
    }
}

impl Drop for DatabaseTransaction<'_> {
    fn drop(&mut self) {
        if !self.done && self.client.batch_execute("ROLLBACK").is_err() {
            self.client.driver.connection.take();
        }
    }
}

impl Deref for DatabaseTransaction<'_> {
    type Target = DatabaseClient;

    fn deref(&self) -> &Self::Target {
        self.client
    }
}

impl DerefMut for DatabaseTransaction<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.client
    }
}
