use std::fmt;

use std::time::{Duration, SystemTime};

use postgres::{Client, NoTls, Row};
use sha2::{Digest, Sha256};

use crate::ipc::{RequesterMetadata, MAX_IPC_COMPONENT_BYTES};

const MIGRATION_LOCK_KEY: i64 = 0x5445_4c43_4841_5201_u64 as i64;
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
        sql: include_str!("../migrations/0001_minimum_lifecycle.sql"),
    },
    Migration {
        version: 2,
        name: "output_retention",
        sql: include_str!("../migrations/0002_output_retention.sql"),
    },
    Migration {
        version: 3,
        name: "execution_state",
        sql: include_str!("../migrations/0003_execution_state.sql"),
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

pub fn migrate(database_url: &str) -> Result<MigrationOutcome, MigrationError> {
    migrate_list(database_url, MIGRATIONS)
}

pub fn requester_reference(requester: &RequesterMetadata) -> String {
    let mut digest = Sha256::new();
    digest.update(b"telchar-requester-v1\0");
    digest.update(requester.credential_id.as_bytes());
    digest.update(b"\0");
    digest.update(requester.audit_subject.as_bytes());
    digest.update(b"\0");
    digest.update(requester.quota_subject.as_bytes());
    format!("{:x}", digest.finalize())
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BuildRequestFailure {
    Configuration,
    Connection,
    Conflict,
    Query,
    Commit,
}

impl BuildRequestFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Connection => "connection",
            Self::Conflict => "conflict",
            Self::Query => "query",
            Self::Commit => "commit",
        }
    }
}

#[derive(Debug)]
pub struct BuildRequestError(BuildRequestFailure);

impl BuildRequestError {
    pub fn failure(&self) -> BuildRequestFailure {
        self.0
    }
}

impl fmt::Display for BuildRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("build request state operation failed")
    }
}

impl std::error::Error for BuildRequestError {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BuildQueueState {
    Accepted,
    Queued,
    Dispatching,
    BackendPending,
    Running,
    Collecting,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BuildRequestState {
    pub request_id: String,
    pub derivation_path: String,
    pub system: String,
    pub queue_state: BuildQueueState,
    pub queued_at: Option<SystemTime>,
    pub audit_subject: String,
    pub quota_subject: String,
    pub created_at: SystemTime,
}

pub fn create_build_request(
    database_url: &str,
    request_id: &str,
    derivation_path: &str,
    system: &str,
    audit_subject: &str,
    quota_subject: &str,
) -> Result<BuildRequestState, BuildRequestError> {
    validate_build_request_inputs(
        database_url,
        request_id,
        derivation_path,
        system,
        audit_subject,
        quota_subject,
    )?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| BuildRequestError(BuildRequestFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| BuildRequestError(BuildRequestFailure::Connection))?;
    let row = transaction
        .query_one(
            "INSERT INTO build_requests (request_id, derivation_path, system, queue_state, queued_at, audit_subject, quota_subject, created_at)
             VALUES ($1, $2, $3, 'accepted', NULL, $4, $5, transaction_timestamp())
             RETURNING request_id, derivation_path, system, queue_state, queued_at, audit_subject, quota_subject, created_at",
            &[&request_id, &derivation_path, &system, &audit_subject, &quota_subject],
        )
        .map_err(|error| BuildRequestError(if error.as_db_error().is_some_and(|database| database.code() == &postgres::error::SqlState::UNIQUE_VIOLATION) { BuildRequestFailure::Conflict } else { BuildRequestFailure::Query }))?;
    let request = decode_build_request(&row).map_err(BuildRequestError)?;
    transaction
        .commit()
        .map_err(|_| BuildRequestError(BuildRequestFailure::Commit))?;
    Ok(request)
}

pub fn read_build_request(
    database_url: &str,
    request_id: &str,
) -> Result<Option<BuildRequestState>, BuildRequestError> {
    validate_build_request_id(request_id)?;
    if database_url.trim().is_empty() {
        return Err(BuildRequestError(BuildRequestFailure::Configuration));
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| BuildRequestError(BuildRequestFailure::Connection))?;
    client
        .query_opt(
            "SELECT request_id, derivation_path, system, queue_state, queued_at, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1",
            &[&request_id],
        )
        .map_err(|_| BuildRequestError(BuildRequestFailure::Query))?
        .map(|row| decode_build_request(&row).map_err(BuildRequestError))
        .transpose()
}

fn validate_build_request_inputs(
    database_url: &str,
    request_id: &str,
    derivation_path: &str,
    system: &str,
    audit_subject: &str,
    quota_subject: &str,
) -> Result<(), BuildRequestError> {
    validate_build_request_id(request_id)?;
    if database_url.trim().is_empty()
        || derivation_path.is_empty()
        || derivation_path.len() > nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
        || system.is_empty()
        || system.len() > MAX_IPC_COMPONENT_BYTES
        || audit_subject.is_empty()
        || audit_subject.len() > MAX_IPC_COMPONENT_BYTES
        || quota_subject.is_empty()
        || quota_subject.len() > crate::ipc::MAX_IPC_CREDENTIAL_ID_BYTES
    {
        return Err(BuildRequestError(BuildRequestFailure::Configuration));
    }
    Ok(())
}

fn validate_build_request_id(request_id: &str) -> Result<(), BuildRequestError> {
    if request_id.is_empty() || request_id.len() > MAX_IPC_COMPONENT_BYTES {
        return Err(BuildRequestError(BuildRequestFailure::Configuration));
    }
    Ok(())
}

fn decode_build_request(row: &Row) -> Result<BuildRequestState, BuildRequestFailure> {
    let request_id: String = row.try_get(0).map_err(|_| BuildRequestFailure::Query)?;
    let derivation_path: String = row.try_get(1).map_err(|_| BuildRequestFailure::Query)?;
    let system: String = row.try_get(2).map_err(|_| BuildRequestFailure::Query)?;
    let queue_state: String = row.try_get(3).map_err(|_| BuildRequestFailure::Query)?;
    let queued_at: Option<SystemTime> = row.try_get(4).map_err(|_| BuildRequestFailure::Query)?;
    let audit_subject: String = row.try_get(5).map_err(|_| BuildRequestFailure::Query)?;
    let quota_subject: String = row.try_get(6).map_err(|_| BuildRequestFailure::Query)?;
    let created_at: SystemTime = row.try_get(7).map_err(|_| BuildRequestFailure::Query)?;
    validate_build_request_inputs(
        "validated",
        &request_id,
        &derivation_path,
        &system,
        &audit_subject,
        &quota_subject,
    )
    .map_err(|_| BuildRequestFailure::Query)?;
    if audit_subject.is_empty()
        || audit_subject.len() > MAX_IPC_COMPONENT_BYTES
        || quota_subject.is_empty()
        || quota_subject.len() > crate::ipc::MAX_IPC_CREDENTIAL_ID_BYTES
    {
        return Err(BuildRequestFailure::Query);
    }
    let queue_state = match (queue_state.as_str(), queued_at) {
        ("accepted", None) => BuildQueueState::Accepted,
        ("queued", Some(_)) => BuildQueueState::Queued,
        ("dispatching", Some(_)) => BuildQueueState::Dispatching,
        ("backend-pending", Some(_)) => BuildQueueState::BackendPending,
        ("running", Some(_)) => BuildQueueState::Running,
        ("collecting", Some(_)) => BuildQueueState::Collecting,
        ("completed", _) => BuildQueueState::Completed,
        ("failed", Some(_)) => BuildQueueState::Failed,
        ("cancelled", Some(_)) => BuildQueueState::Cancelled,
        _ => return Err(BuildRequestFailure::Query),
    };
    Ok(BuildRequestState {
        request_id,
        derivation_path,
        system,
        queue_state,
        queued_at,
        audit_subject,
        quota_subject,
        created_at,
    })
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ExecutionAttemptFailure {
    Configuration,
    Connection,
    Conflict,
    Query,
    Commit,
}

impl ExecutionAttemptFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Connection => "connection",
            Self::Conflict => "conflict",
            Self::Query => "query",
            Self::Commit => "commit",
        }
    }
}

#[derive(Debug)]
pub struct ExecutionAttemptError(ExecutionAttemptFailure);

impl ExecutionAttemptError {
    pub fn failure(&self) -> ExecutionAttemptFailure {
        self.0
    }
}

impl fmt::Display for ExecutionAttemptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("execution attempt state operation failed")
    }
}

impl std::error::Error for ExecutionAttemptError {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ExecutionAttemptState {
    Dispatching,
    BackendPending,
    Running,
    Collecting,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ExecutionAttempt {
    pub attempt_id: String,
    pub request_id: String,
    pub ordinal: i32,
    pub idempotency_key: String,
    pub backend: String,
    pub backend_execution_id: Option<String>,
    pub state: ExecutionAttemptState,
    pub created_at: SystemTime,
    pub submitted_at: Option<SystemTime>,
    pub started_at: Option<SystemTime>,
    pub collecting_at: Option<SystemTime>,
    pub completed_at: Option<SystemTime>,
    pub fenced_at: Option<SystemTime>,
}

pub fn create_execution_attempt(
    database_url: &str,
    attempt_id: &str,
    request_id: &str,
    ordinal: i32,
    idempotency_key: &str,
    backend: &str,
) -> Result<ExecutionAttempt, ExecutionAttemptError> {
    validate_execution_attempt_inputs(
        database_url,
        attempt_id,
        request_id,
        ordinal,
        idempotency_key,
        backend,
    )?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| ExecutionAttemptError(ExecutionAttemptFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| ExecutionAttemptError(ExecutionAttemptFailure::Connection))?;
    let row = transaction
        .query_one(
            "INSERT INTO execution_attempts (attempt_id, request_id, ordinal, idempotency_key, backend, backend_execution_id, state, created_at) VALUES ($1, $2, $3, $4, $5, NULL, 'dispatching', transaction_timestamp()) RETURNING attempt_id, request_id, ordinal, idempotency_key, backend, backend_execution_id, state, created_at, submitted_at, started_at, collecting_at, completed_at, fenced_at",
            &[&attempt_id, &request_id, &ordinal, &idempotency_key, &backend],
        )
        .map_err(|error| {
            ExecutionAttemptError(if error.as_db_error().is_some_and(|database| {
                database.code() == &postgres::error::SqlState::UNIQUE_VIOLATION
            }) {
                ExecutionAttemptFailure::Conflict
            } else {
                ExecutionAttemptFailure::Query
            })
        })?;
    let attempt = decode_execution_attempt(&row).map_err(ExecutionAttemptError)?;
    transaction
        .commit()
        .map_err(|_| ExecutionAttemptError(ExecutionAttemptFailure::Commit))?;
    Ok(attempt)
}

pub fn read_execution_attempt(
    database_url: &str,
    attempt_id: &str,
) -> Result<Option<ExecutionAttempt>, ExecutionAttemptError> {
    validate_execution_attempt_component(database_url, attempt_id)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| ExecutionAttemptError(ExecutionAttemptFailure::Connection))?;
    client
        .query_opt(
            "SELECT attempt_id, request_id, ordinal, idempotency_key, backend, backend_execution_id, state, created_at, submitted_at, started_at, collecting_at, completed_at, fenced_at FROM execution_attempts WHERE attempt_id = $1",
            &[&attempt_id],
        )
        .map_err(|_| ExecutionAttemptError(ExecutionAttemptFailure::Query))?
        .map(|row| decode_execution_attempt(&row).map_err(ExecutionAttemptError))
        .transpose()
}

fn validate_execution_attempt_inputs(
    database_url: &str,
    attempt_id: &str,
    request_id: &str,
    ordinal: i32,
    idempotency_key: &str,
    backend: &str,
) -> Result<(), ExecutionAttemptError> {
    validate_execution_attempt_component(database_url, attempt_id)?;
    if request_id.is_empty()
        || request_id.len() > MAX_IPC_COMPONENT_BYTES
        || ordinal <= 0
        || idempotency_key.is_empty()
        || idempotency_key.len() > MAX_IPC_COMPONENT_BYTES
        || backend.is_empty()
        || backend.len() > MAX_IPC_COMPONENT_BYTES
    {
        return Err(ExecutionAttemptError(
            ExecutionAttemptFailure::Configuration,
        ));
    }
    Ok(())
}

fn validate_execution_attempt_component(
    database_url: &str,
    value: &str,
) -> Result<(), ExecutionAttemptError> {
    if database_url.trim().is_empty() || value.is_empty() || value.len() > MAX_IPC_COMPONENT_BYTES {
        return Err(ExecutionAttemptError(
            ExecutionAttemptFailure::Configuration,
        ));
    }
    Ok(())
}

fn decode_execution_attempt(row: &Row) -> Result<ExecutionAttempt, ExecutionAttemptFailure> {
    let attempt_id: String = row.try_get(0).map_err(|_| ExecutionAttemptFailure::Query)?;
    let request_id: String = row.try_get(1).map_err(|_| ExecutionAttemptFailure::Query)?;
    let ordinal: i32 = row.try_get(2).map_err(|_| ExecutionAttemptFailure::Query)?;
    let idempotency_key: String = row.try_get(3).map_err(|_| ExecutionAttemptFailure::Query)?;
    let backend: String = row.try_get(4).map_err(|_| ExecutionAttemptFailure::Query)?;
    let backend_execution_id: Option<String> =
        row.try_get(5).map_err(|_| ExecutionAttemptFailure::Query)?;
    let state: String = row.try_get(6).map_err(|_| ExecutionAttemptFailure::Query)?;
    let created_at: SystemTime = row.try_get(7).map_err(|_| ExecutionAttemptFailure::Query)?;
    let submitted_at: Option<SystemTime> =
        row.try_get(8).map_err(|_| ExecutionAttemptFailure::Query)?;
    let started_at: Option<SystemTime> =
        row.try_get(9).map_err(|_| ExecutionAttemptFailure::Query)?;
    let collecting_at: Option<SystemTime> = row
        .try_get(10)
        .map_err(|_| ExecutionAttemptFailure::Query)?;
    let completed_at: Option<SystemTime> = row
        .try_get(11)
        .map_err(|_| ExecutionAttemptFailure::Query)?;
    let fenced_at: Option<SystemTime> = row
        .try_get(12)
        .map_err(|_| ExecutionAttemptFailure::Query)?;
    validate_execution_attempt_inputs(
        "validated",
        &attempt_id,
        &request_id,
        ordinal,
        &idempotency_key,
        &backend,
    )
    .map_err(|_| ExecutionAttemptFailure::Query)?;
    if backend_execution_id
        .as_ref()
        .is_some_and(|value: &String| value.is_empty() || value.len() > MAX_IPC_COMPONENT_BYTES)
    {
        return Err(ExecutionAttemptFailure::Query);
    }
    let state = match state.as_str() {
        "dispatching"
            if submitted_at.is_none()
                && started_at.is_none()
                && collecting_at.is_none()
                && completed_at.is_none() =>
        {
            ExecutionAttemptState::Dispatching
        }
        "backend-pending"
            if submitted_at.is_some()
                && started_at.is_none()
                && collecting_at.is_none()
                && completed_at.is_none() =>
        {
            ExecutionAttemptState::BackendPending
        }
        "running"
            if submitted_at.is_some()
                && started_at.is_some()
                && collecting_at.is_none()
                && completed_at.is_none() =>
        {
            ExecutionAttemptState::Running
        }
        "collecting"
            if submitted_at.is_some()
                && started_at.is_some()
                && collecting_at.is_some()
                && completed_at.is_none() =>
        {
            ExecutionAttemptState::Collecting
        }
        "succeeded" if completed_at.is_some() => ExecutionAttemptState::Succeeded,
        "failed" if completed_at.is_some() => ExecutionAttemptState::Failed,
        "cancelled" if completed_at.is_some() => ExecutionAttemptState::Cancelled,
        _ => return Err(ExecutionAttemptFailure::Query),
    };
    Ok(ExecutionAttempt {
        attempt_id,
        request_id,
        ordinal,
        idempotency_key,
        backend,
        backend_execution_id,
        state,
        created_at,
        submitted_at,
        started_at,
        collecting_at,
        completed_at,
        fenced_at,
    })
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RequestAttachmentFailure {
    Configuration,
    Connection,
    Conflict,
    Missing,
    InvalidState,
    Query,
    Commit,
}

impl RequestAttachmentFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Connection => "connection",
            Self::Conflict => "conflict",
            Self::Missing => "missing",
            Self::InvalidState => "invalid_state",
            Self::Query => "query",
            Self::Commit => "commit",
        }
    }
}

#[derive(Debug)]
pub struct RequestAttachmentError(RequestAttachmentFailure);

impl RequestAttachmentError {
    pub fn failure(&self) -> RequestAttachmentFailure {
        self.0
    }
}

impl fmt::Display for RequestAttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("request attachment state operation failed")
    }
}

impl std::error::Error for RequestAttachmentError {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RequestAttachmentState {
    Attached,
    Detached,
    Delivered,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RequestAttachment {
    pub session_id: String,
    pub request_id: String,
    pub state: RequestAttachmentState,
    pub attached_at: SystemTime,
    pub detached_at: Option<SystemTime>,
    pub delivered_at: Option<SystemTime>,
}

pub fn attach_request(
    database_url: &str,
    session_id: &str,
    request_id: &str,
) -> Result<RequestAttachment, RequestAttachmentError> {
    validate_request_attachment_inputs(database_url, session_id, request_id)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Connection))?;
    let session = transaction
        .query_opt(
            "SELECT session_id, requester_reference, audit_subject, quota_subject, state, created_at, closed_at FROM protocol_sessions WHERE session_id = $1 FOR NO KEY UPDATE",
            &[&session_id],
        )
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Query))?;
    match session {
        None => return Err(RequestAttachmentError(RequestAttachmentFailure::Missing)),
        Some(row) => match decode_protocol_session(&row) {
            Ok(ProtocolSession {
                state: ProtocolSessionState::Open,
                ..
            }) => {}
            Ok(_) => {
                return Err(RequestAttachmentError(
                    RequestAttachmentFailure::InvalidState,
                ));
            }
            Err(_) => return Err(RequestAttachmentError(RequestAttachmentFailure::Query)),
        },
    }
    match transaction.query_opt("SELECT request_id, derivation_path, system, queue_state, queued_at, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1", &[&request_id])
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Query))?
    {
        None => return Err(RequestAttachmentError(RequestAttachmentFailure::Missing)),
        Some(row) if decode_build_request(&row).is_ok() => {}
        Some(_) => return Err(RequestAttachmentError(RequestAttachmentFailure::Query)),
    }
    let row = transaction
        .query_one(
            "INSERT INTO request_attachments (session_id, request_id, state, attached_at, detached_at, delivered_at) VALUES ($1, $2, 'attached', transaction_timestamp(), NULL, NULL) RETURNING session_id, request_id, state, attached_at, detached_at, delivered_at",
            &[&session_id, &request_id],
        )
        .map_err(|error| RequestAttachmentError(if error.as_db_error().is_some_and(|database| database.code() == &postgres::error::SqlState::UNIQUE_VIOLATION) { RequestAttachmentFailure::Conflict } else { RequestAttachmentFailure::Query }))?;
    let attachment = decode_request_attachment(&row).map_err(RequestAttachmentError)?;
    transaction
        .commit()
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Commit))?;
    Ok(attachment)
}

pub fn detach_request(
    database_url: &str,
    session_id: &str,
    request_id: &str,
) -> Result<RequestAttachment, RequestAttachmentError> {
    validate_request_attachment_inputs(database_url, session_id, request_id)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Connection))?;
    let row = transaction
        .query_opt(
            "UPDATE request_attachments SET state = 'detached', detached_at = transaction_timestamp() WHERE session_id = $1 AND request_id = $2 AND state = 'attached' RETURNING session_id, request_id, state, attached_at, detached_at, delivered_at",
            &[&session_id, &request_id],
        )
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Query))?;
    let attachment = match row {
        Some(row) => decode_request_attachment(&row).map_err(RequestAttachmentError)?,
        None => match transaction
            .query_opt(
                "SELECT state FROM request_attachments WHERE session_id = $1 AND request_id = $2",
                &[&session_id, &request_id],
            )
            .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Query))?
        {
            None => return Err(RequestAttachmentError(RequestAttachmentFailure::Missing)),
            Some(row)
                if matches!(
                    row.try_get::<_, String>(0).ok().as_deref(),
                    Some("detached" | "delivered")
                ) =>
            {
                return Err(RequestAttachmentError(
                    RequestAttachmentFailure::InvalidState,
                ));
            }
            Some(row) if row.try_get::<_, String>(0).is_ok() => {
                return Err(RequestAttachmentError(RequestAttachmentFailure::Query));
            }
            Some(_) => return Err(RequestAttachmentError(RequestAttachmentFailure::Query)),
        },
    };
    transaction
        .commit()
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Commit))?;
    Ok(attachment)
}

pub fn complete_request_delivery(
    database_url: &str,
    session_id: &str,
    request_id: &str,
) -> Result<RequestAttachment, RequestAttachmentError> {
    validate_request_attachment_inputs(database_url, session_id, request_id)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Connection))?;
    let row = transaction
        .query_opt(
            "UPDATE request_attachments SET state = 'delivered', delivered_at = transaction_timestamp() WHERE session_id = $1 AND request_id = $2 AND state = 'attached' RETURNING session_id, request_id, state, attached_at, detached_at, delivered_at",
            &[&session_id, &request_id],
        )
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Query))?;
    let attachment = match row {
        Some(row) => decode_request_attachment(&row).map_err(RequestAttachmentError)?,
        None => {
            let state = transaction
                .query_opt(
                    "SELECT state FROM request_attachments WHERE session_id = $1 AND request_id = $2",
                    &[&session_id, &request_id],
                )
                .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Query))?;
            return Err(RequestAttachmentError(match state {
                None => RequestAttachmentFailure::Missing,
                Some(_) => RequestAttachmentFailure::InvalidState,
            }));
        }
    };
    transaction
        .commit()
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Commit))?;
    Ok(attachment)
}

pub fn read_request_attachment(
    database_url: &str,
    session_id: &str,
    request_id: &str,
) -> Result<Option<RequestAttachment>, RequestAttachmentError> {
    validate_request_attachment_inputs(database_url, session_id, request_id)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Connection))?;
    client
        .query_opt(
            "SELECT session_id, request_id, state, attached_at, detached_at, delivered_at FROM request_attachments WHERE session_id = $1 AND request_id = $2",
            &[&session_id, &request_id],
        )
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Query))?
        .map(|row| decode_request_attachment(&row).map_err(RequestAttachmentError))
        .transpose()
}

fn validate_request_attachment_inputs(
    database_url: &str,
    session_id: &str,
    request_id: &str,
) -> Result<(), RequestAttachmentError> {
    if database_url.trim().is_empty()
        || session_id.is_empty()
        || session_id.len() > MAX_IPC_COMPONENT_BYTES
        || request_id.is_empty()
        || request_id.len() > MAX_IPC_COMPONENT_BYTES
    {
        return Err(RequestAttachmentError(
            RequestAttachmentFailure::Configuration,
        ));
    }
    Ok(())
}

fn decode_request_attachment(row: &Row) -> Result<RequestAttachment, RequestAttachmentFailure> {
    let session_id: String = row
        .try_get(0)
        .map_err(|_| RequestAttachmentFailure::Query)?;
    let request_id: String = row
        .try_get(1)
        .map_err(|_| RequestAttachmentFailure::Query)?;
    let state: String = row
        .try_get(2)
        .map_err(|_| RequestAttachmentFailure::Query)?;
    let attached_at: SystemTime = row
        .try_get(3)
        .map_err(|_| RequestAttachmentFailure::Query)?;
    let detached_at: Option<SystemTime> = row
        .try_get(4)
        .map_err(|_| RequestAttachmentFailure::Query)?;
    let delivered_at: Option<SystemTime> = row
        .try_get(5)
        .map_err(|_| RequestAttachmentFailure::Query)?;
    validate_request_attachment_inputs("validated", &session_id, &request_id)
        .map_err(|_| RequestAttachmentFailure::Query)?;
    let state = match state.as_str() {
        "attached" if detached_at.is_none() && delivered_at.is_none() => {
            RequestAttachmentState::Attached
        }
        "detached"
            if detached_at.is_some_and(|detached_at| detached_at >= attached_at)
                && delivered_at.is_none() =>
        {
            RequestAttachmentState::Detached
        }
        "delivered"
            if detached_at.is_none()
                && delivered_at.is_some_and(|delivered_at| delivered_at >= attached_at) =>
        {
            RequestAttachmentState::Delivered
        }
        _ => return Err(RequestAttachmentFailure::Query),
    };
    Ok(RequestAttachment {
        session_id,
        request_id,
        state,
        attached_at,
        detached_at,
        delivered_at,
    })
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ProtocolSessionFailure {
    Configuration,
    Connection,
    Conflict,
    NotFound,
    InvalidTransition,
    Query,
    Commit,
}

impl ProtocolSessionFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Connection => "connection",
            Self::Conflict => "conflict",
            Self::NotFound => "not-found",
            Self::InvalidTransition => "invalid-transition",
            Self::Query => "query",
            Self::Commit => "commit",
        }
    }
}

#[derive(Debug)]
pub struct ProtocolSessionError(ProtocolSessionFailure);

impl ProtocolSessionError {
    pub fn failure(&self) -> ProtocolSessionFailure {
        self.0
    }
}

impl fmt::Display for ProtocolSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("protocol session state operation failed")
    }
}

impl std::error::Error for ProtocolSessionError {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ProtocolSessionState {
    Open,
    Closed,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProtocolSession {
    pub session_id: String,
    pub requester_reference: String,
    pub audit_subject: String,
    pub quota_subject: String,
    pub state: ProtocolSessionState,
    pub created_at: SystemTime,
    pub closed_at: Option<SystemTime>,
}

pub fn open_protocol_session(
    database_url: &str,
    session_id: &str,
    requester_reference: &str,
    audit_subject: &str,
    quota_subject: &str,
) -> Result<ProtocolSession, ProtocolSessionError> {
    validate_protocol_session_inputs(
        database_url,
        session_id,
        requester_reference,
        audit_subject,
        quota_subject,
    )?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Connection))?;
    let row = transaction
        .query_one(
            "INSERT INTO protocol_sessions (session_id, requester_reference, audit_subject, quota_subject, state, created_at, closed_at) VALUES ($1, $2, $3, $4, 'open', transaction_timestamp(), NULL) RETURNING session_id, requester_reference, audit_subject, quota_subject, state, created_at, closed_at",
            &[&session_id, &requester_reference, &audit_subject, &quota_subject],
        )
        .map_err(|error| ProtocolSessionError(if error.as_db_error().is_some_and(|database| database.code() == &postgres::error::SqlState::UNIQUE_VIOLATION) { ProtocolSessionFailure::Conflict } else { ProtocolSessionFailure::Query }))?;
    let session = decode_protocol_session(&row).map_err(ProtocolSessionError)?;
    transaction
        .commit()
        .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Commit))?;
    Ok(session)
}

pub fn close_protocol_session(
    database_url: &str,
    session_id: &str,
) -> Result<ProtocolSession, ProtocolSessionError> {
    validate_session_id(session_id)?;
    if database_url.trim().is_empty() {
        return Err(ProtocolSessionError(ProtocolSessionFailure::Configuration));
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Connection))?;
    let row = transaction
        .query_opt(
            "UPDATE protocol_sessions SET state = 'closed', closed_at = transaction_timestamp() WHERE session_id = $1 AND state = 'open' RETURNING session_id, requester_reference, audit_subject, quota_subject, state, created_at, closed_at",
            &[&session_id],
        )
        .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Query))?;
    let session = match row {
        Some(row) => decode_protocol_session(&row).map_err(ProtocolSessionError)?,
        None => {
            let state = transaction
                .query_opt(
                    "SELECT state FROM protocol_sessions WHERE session_id = $1",
                    &[&session_id],
                )
                .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Query))?;
            return Err(ProtocolSessionError(match state {
                None => ProtocolSessionFailure::NotFound,
                Some(_) => ProtocolSessionFailure::InvalidTransition,
            }));
        }
    };
    transaction
        .commit()
        .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Commit))?;
    Ok(session)
}

pub fn read_protocol_session(
    database_url: &str,
    session_id: &str,
) -> Result<Option<ProtocolSession>, ProtocolSessionError> {
    validate_session_id(session_id)?;
    if database_url.trim().is_empty() {
        return Err(ProtocolSessionError(ProtocolSessionFailure::Configuration));
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Connection))?;
    client
        .query_opt(
            "SELECT session_id, requester_reference, audit_subject, quota_subject, state, created_at, closed_at FROM protocol_sessions WHERE session_id = $1",
            &[&session_id],
        )
        .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Query))?
        .map(|row| decode_protocol_session(&row).map_err(ProtocolSessionError))
        .transpose()
}

fn validate_protocol_session_inputs(
    database_url: &str,
    session_id: &str,
    requester_reference: &str,
    audit_subject: &str,
    quota_subject: &str,
) -> Result<(), ProtocolSessionError> {
    validate_session_id(session_id)?;
    if database_url.trim().is_empty()
        || !is_requester_reference(requester_reference)
        || audit_subject.is_empty()
        || audit_subject.len() > MAX_IPC_COMPONENT_BYTES
        || quota_subject.is_empty()
        || quota_subject.len() > crate::ipc::MAX_IPC_CREDENTIAL_ID_BYTES
    {
        return Err(ProtocolSessionError(ProtocolSessionFailure::Configuration));
    }
    Ok(())
}

fn validate_session_id(session_id: &str) -> Result<(), ProtocolSessionError> {
    if session_id.is_empty() || session_id.len() > MAX_IPC_COMPONENT_BYTES {
        return Err(ProtocolSessionError(ProtocolSessionFailure::Configuration));
    }
    Ok(())
}

fn is_requester_reference(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn decode_protocol_session(row: &Row) -> Result<ProtocolSession, ProtocolSessionFailure> {
    let session_id: String = row.try_get(0).map_err(|_| ProtocolSessionFailure::Query)?;
    let requester_reference: String = row.try_get(1).map_err(|_| ProtocolSessionFailure::Query)?;
    let audit_subject: String = row.try_get(2).map_err(|_| ProtocolSessionFailure::Query)?;
    let quota_subject: String = row.try_get(3).map_err(|_| ProtocolSessionFailure::Query)?;
    let state: String = row.try_get(4).map_err(|_| ProtocolSessionFailure::Query)?;
    let created_at: SystemTime = row.try_get(5).map_err(|_| ProtocolSessionFailure::Query)?;
    let closed_at: Option<SystemTime> =
        row.try_get(6).map_err(|_| ProtocolSessionFailure::Query)?;
    let state = match state.as_str() {
        "open"
            if closed_at.is_none()
                && is_requester_reference(&requester_reference)
                && !audit_subject.is_empty()
                && audit_subject.len() <= MAX_IPC_COMPONENT_BYTES
                && !quota_subject.is_empty()
                && quota_subject.len() <= crate::ipc::MAX_IPC_CREDENTIAL_ID_BYTES =>
        {
            ProtocolSessionState::Open
        }
        "closed"
            if closed_at.is_some_and(|closed_at| closed_at >= created_at)
                && is_requester_reference(&requester_reference)
                && !audit_subject.is_empty()
                && audit_subject.len() <= MAX_IPC_COMPONENT_BYTES
                && !quota_subject.is_empty()
                && quota_subject.len() <= crate::ipc::MAX_IPC_CREDENTIAL_ID_BYTES =>
        {
            ProtocolSessionState::Closed
        }
        _ => return Err(ProtocolSessionFailure::Query),
    };
    Ok(ProtocolSession {
        session_id,
        requester_reference,
        audit_subject,
        quota_subject,
        state,
        created_at,
        closed_at,
    })
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StoreLeaseFailure {
    Configuration,
    Connection,
    Conflict,
    Missing,
    InvalidState,
    Query,
    Commit,
}

impl StoreLeaseFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Connection => "connection",
            Self::Conflict => "conflict",
            Self::Missing => "missing",
            Self::InvalidState => "invalid_state",
            Self::Query => "query",
            Self::Commit => "commit",
        }
    }
}

#[derive(Debug)]
pub struct StoreLeaseError(StoreLeaseFailure);

impl StoreLeaseError {
    pub fn failure(&self) -> StoreLeaseFailure {
        self.0
    }
}

impl fmt::Display for StoreLeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("store lease state operation failed")
    }
}

impl std::error::Error for StoreLeaseError {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StoreLeaseOwnerKind {
    Session,
    Request,
}

impl StoreLeaseOwnerKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Request => "request",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "session" => Some(Self::Session),
            "request" => Some(Self::Request),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StoreLeasePurpose {
    Derivation,
    Input,
    Output,
    Transfer,
}

impl StoreLeasePurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Derivation => "derivation",
            Self::Input => "input",
            Self::Output => "output",
            Self::Transfer => "transfer",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "derivation" => Some(Self::Derivation),
            "input" => Some(Self::Input),
            "output" => Some(Self::Output),
            "transfer" => Some(Self::Transfer),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StoreLeaseState {
    Active,
    Released,
}

impl StoreLeaseState {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "released" => Some(Self::Released),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StoreLeaseRecord {
    pub lease_id: String,
    pub owner_kind: StoreLeaseOwnerKind,
    pub owner_id: String,
    pub store_path: String,
    pub purpose: StoreLeasePurpose,
    pub state: StoreLeaseState,
    pub created_at: SystemTime,
    pub released_at: Option<SystemTime>,
    pub expires_at: Option<SystemTime>,
}

pub fn create_store_lease(
    database_url: &str,
    lease_id: &str,
    owner_kind: StoreLeaseOwnerKind,
    owner_id: &str,
    store_path: &str,
    purpose: StoreLeasePurpose,
) -> Result<StoreLeaseRecord, StoreLeaseError> {
    let result = create_store_lease_inner(
        database_url,
        lease_id,
        owner_kind,
        owner_id,
        store_path,
        purpose,
    );
    emit_store_lease_failure("create", &result);
    result
}

fn create_store_lease_inner(
    database_url: &str,
    lease_id: &str,
    owner_kind: StoreLeaseOwnerKind,
    owner_id: &str,
    store_path: &str,
    purpose: StoreLeasePurpose,
) -> Result<StoreLeaseRecord, StoreLeaseError> {
    validate_store_lease_inputs(database_url, lease_id, owner_id, store_path)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    match owner_kind {
        StoreLeaseOwnerKind::Request => match transaction.query_opt("SELECT request_id, derivation_path, system, queue_state, queued_at, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1", &[&owner_id])
            .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        {
            None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
            Some(row) if decode_build_request(&row).is_ok() => {}
            Some(_) => return Err(StoreLeaseError(StoreLeaseFailure::Query)),
        },
        StoreLeaseOwnerKind::Session => match transaction
            .query_opt(
                "SELECT session_id, requester_reference, audit_subject, quota_subject, state, created_at, closed_at FROM protocol_sessions WHERE session_id = $1 FOR NO KEY UPDATE",
                &[&owner_id],
            )
            .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        {
            None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
            Some(row) => match decode_protocol_session(&row) {
                Ok(ProtocolSession {
                    state: ProtocolSessionState::Open,
                    ..
                }) => {}
                Ok(_) => return Err(StoreLeaseError(StoreLeaseFailure::InvalidState)),
                Err(_) => return Err(StoreLeaseError(StoreLeaseFailure::Query)),
            },
        },
    }
    let row = transaction
        .query_one(
            "INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at) VALUES ($1, $2, $3, $4, $5, 'active', transaction_timestamp(), NULL, CASE WHEN $5 = 'output' THEN transaction_timestamp() + interval '1 hour' ELSE NULL END) RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at",
            &[&lease_id, &owner_kind.as_str(), &owner_id, &store_path, &purpose.as_str()],
        )
        .map_err(|error| StoreLeaseError(if error.as_db_error().is_some_and(|database| database.code() == &postgres::error::SqlState::UNIQUE_VIOLATION) { StoreLeaseFailure::Conflict } else { StoreLeaseFailure::Query }))?;
    let lease = decode_store_lease(&row).map_err(StoreLeaseError)?;
    transaction
        .commit()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Commit))?;
    tracing::info!(
        event = "database.store_lease.created",
        operation = "create",
        owner_kind = owner_kind.as_str(),
        purpose = purpose.as_str(),
        state = "active"
    );
    Ok(lease)
}

pub fn create_request_input_leases(
    database_url: &str,
    request_id: &str,
    leases: &[(String, String)],
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    if leases.is_empty() {
        return Ok(Vec::new());
    }
    let result = create_request_input_leases_inner(database_url, request_id, leases);
    emit_store_lease_batch_failure(
        result.as_ref().map(|_| ()),
        leases
            .len()
            .min(nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES),
    );
    result
}

fn create_request_input_leases_inner(
    database_url: &str,
    request_id: &str,
    leases: &[(String, String)],
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    if database_url.trim().is_empty()
        || request_id.is_empty()
        || request_id.len() > MAX_IPC_COMPONENT_BYTES
        || leases.len() > nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES
    {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    let mut ids = std::collections::HashSet::new();
    let mut paths = std::collections::HashSet::new();
    for (lease_id, store_path) in leases {
        validate_store_lease_id(lease_id)?;
        validate_store_lease_inputs("validated", lease_id, request_id, store_path)?;
        if !ids.insert(lease_id) || !paths.insert(store_path) {
            return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
        }
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let request = transaction.query_opt("SELECT request_id, derivation_path, system, queue_state, queued_at, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1 FOR NO KEY UPDATE", &[&request_id])
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match request {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => {
            decode_build_request(&row).map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
        }
    }
    let mut records = Vec::with_capacity(leases.len());
    for (lease_id, store_path) in leases {
        let row = transaction
            .query_one(
                "INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at) VALUES ($1, 'request', $2, $3, 'input', 'active', transaction_timestamp(), NULL, NULL) RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at",
                &[&lease_id, &request_id, &store_path],
            )
            .map_err(|error| StoreLeaseError(if error.as_db_error().is_some_and(|database| database.code() == &postgres::error::SqlState::UNIQUE_VIOLATION) { StoreLeaseFailure::Conflict } else { StoreLeaseFailure::Query }))?;
        records.push(decode_store_lease(&row).map_err(StoreLeaseError)?);
    }
    transaction
        .commit()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Commit))?;
    tracing::info!(
        event = "database.store_lease.created",
        operation = "create-input-closure",
        owner_kind = "request",
        purpose = "input",
        state = "active",
        path_count = records.len(),
        "store lease batch persisted"
    );
    Ok(records)
}

fn emit_store_lease_batch_failure(result: Result<(), &StoreLeaseError>, path_count: usize) {
    if let Err(error) = result {
        tracing::warn!(
            event = "database.store_lease.failed",
            operation = "create-input-closure",
            owner_kind = "request",
            purpose = "input",
            state = "active",
            path_count,
            failure_class = error.failure().as_str(),
            "store lease persistence failed"
        );
    }
}

pub fn create_request_output_leases(
    database_url: &str,
    request_id: &str,
    retention: Duration,
    leases: &[(String, String)],
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    let retention_seconds = retention.as_secs();
    if retention.subsec_nanos() != 0 || !(60..=86_400).contains(&retention_seconds) {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    if leases.is_empty() {
        return Ok(Vec::new());
    }
    let result =
        create_request_output_leases_inner(database_url, request_id, retention_seconds, leases);
    emit_request_output_lease_batch_result(result.as_ref().map(|records| records.len()), &result);
    result
}

fn create_request_output_leases_inner(
    database_url: &str,
    request_id: &str,
    retention_seconds: u64,
    leases: &[(String, String)],
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    if database_url.trim().is_empty()
        || request_id.is_empty()
        || request_id.len() > MAX_IPC_COMPONENT_BYTES
        || leases.len() > nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_OUTPUTS
    {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    let mut ids = std::collections::HashSet::new();
    let mut paths = std::collections::HashSet::new();
    for (lease_id, store_path) in leases {
        validate_store_lease_id(lease_id)?;
        validate_store_lease_inputs("validated", lease_id, request_id, store_path)?;
        if !ids.insert(lease_id) || !paths.insert(store_path) {
            return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
        }
    }
    let retention_seconds = retention_seconds as f64;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let request = transaction.query_opt("SELECT request_id, derivation_path, system, queue_state, queued_at, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1 FOR NO KEY UPDATE", &[&request_id])
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match request {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => {
            decode_build_request(&row).map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
        }
    }
    let mut records = Vec::with_capacity(leases.len());
    for (lease_id, store_path) in leases {
        let row = transaction
            .query_one(
                "INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at) VALUES ($1, 'request', $2, $3, 'output', 'active', transaction_timestamp(), NULL, transaction_timestamp() + make_interval(secs => $4)) RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at",
                &[&lease_id, &request_id, &store_path, &retention_seconds],
            )
            .map_err(|error| StoreLeaseError(if error.as_db_error().is_some_and(|database| database.code() == &postgres::error::SqlState::UNIQUE_VIOLATION) { StoreLeaseFailure::Conflict } else { StoreLeaseFailure::Query }))?;
        records.push(decode_store_lease(&row).map_err(StoreLeaseError)?);
    }
    transaction
        .commit()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Commit))?;
    Ok(records)
}

fn emit_request_output_lease_batch_result(
    path_count: Result<usize, &StoreLeaseError>,
    result: &Result<Vec<StoreLeaseRecord>, StoreLeaseError>,
) {
    match result {
        Ok(_) => tracing::info!(
            event = "database.store_lease.created",
            operation = "create-output-retention",
            result = "succeeded",
            path_count = path_count.unwrap_or(0),
            "store lease batch persisted"
        ),
        Err(error) => tracing::warn!(
            event = "database.store_lease.failed",
            operation = "create-output-retention",
            result = "failed",
            path_count = 0_usize,
            failure_class = error.failure().as_str(),
            "store lease persistence failed"
        ),
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReleasedRequestLeases {
    pub leases: Vec<StoreLeaseRecord>,
}

pub fn detach_request_and_release_leases(
    database_url: &str,
    session_id: &str,
    request_id: &str,
) -> Result<ReleasedRequestLeases, StoreLeaseError> {
    validate_request_lease_release_inputs(database_url, session_id, request_id)?;
    let result = detach_request_and_release_leases_inner(database_url, session_id, request_id);
    emit_request_lease_release_result(
        "detach-release",
        result.as_ref().map(|released| released.leases.len()),
        &result,
    );
    result
}

fn detach_request_and_release_leases_inner(
    database_url: &str,
    session_id: &str,
    request_id: &str,
) -> Result<ReleasedRequestLeases, StoreLeaseError> {
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let request = transaction.query_opt("SELECT request_id, derivation_path, system, queue_state, queued_at, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1 FOR UPDATE", &[&request_id])
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match request {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => {
            decode_build_request(&row).map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        }
    };
    let attachment = transaction
        .query_opt(
            "SELECT session_id, request_id, state, attached_at, detached_at, delivered_at FROM request_attachments WHERE session_id = $1 AND request_id = $2 FOR UPDATE",
            &[&session_id, &request_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match attachment {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => match decode_request_attachment(&row) {
            Ok(RequestAttachment {
                state: RequestAttachmentState::Attached,
                ..
            }) => {}
            Ok(_) => return Err(StoreLeaseError(StoreLeaseFailure::InvalidState)),
            Err(_) => return Err(StoreLeaseError(StoreLeaseFailure::Query)),
        },
    }
    let locked = lock_active_request_leases(&mut transaction, request_id)?;
    let attachment = transaction
        .query_one(
            "UPDATE request_attachments SET state = 'detached', detached_at = transaction_timestamp() WHERE session_id = $1 AND request_id = $2 AND state = 'attached' RETURNING session_id, request_id, state, attached_at, detached_at, delivered_at",
            &[&session_id, &request_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match decode_request_attachment(&attachment) {
        Ok(RequestAttachment {
            state: RequestAttachmentState::Detached,
            ..
        }) => {}
        _ => return Err(StoreLeaseError(StoreLeaseFailure::Query)),
    }
    let released = release_locked_request_leases(&mut transaction, request_id, &locked)?;
    transaction
        .commit()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Commit))?;
    Ok(ReleasedRequestLeases { leases: released })
}

pub fn release_unattached_request_leases(
    database_url: &str,
    request_id: &str,
) -> Result<ReleasedRequestLeases, StoreLeaseError> {
    if database_url.trim().is_empty()
        || request_id.is_empty()
        || request_id.len() > MAX_IPC_COMPONENT_BYTES
    {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    let result = release_unattached_request_leases_inner(database_url, request_id);
    emit_request_lease_release_result(
        "unattached-release",
        result.as_ref().map(|released| released.leases.len()),
        &result,
    );
    result
}

fn release_unattached_request_leases_inner(
    database_url: &str,
    request_id: &str,
) -> Result<ReleasedRequestLeases, StoreLeaseError> {
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let request = transaction.query_opt("SELECT request_id, derivation_path, system, queue_state, queued_at, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1 FOR UPDATE", &[&request_id])
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match request {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => {
            decode_build_request(&row).map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        }
    };
    if transaction
        .query_opt(
            "SELECT session_id, request_id, state, attached_at, detached_at, delivered_at FROM request_attachments WHERE request_id = $1 FOR UPDATE",
            &[&request_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        .is_some()
    {
        return Err(StoreLeaseError(StoreLeaseFailure::InvalidState));
    }
    let locked = lock_active_request_leases(&mut transaction, request_id)?;
    let released = release_locked_request_leases(&mut transaction, request_id, &locked)?;
    transaction
        .commit()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Commit))?;
    Ok(ReleasedRequestLeases { leases: released })
}

fn lock_active_request_leases(
    transaction: &mut postgres::Transaction<'_>,
    request_id: &str,
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    let rows = transaction
        .query(
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at FROM store_leases WHERE owner_kind = 'request' AND owner_id = $1 ORDER BY lease_id FOR UPDATE",
            &[&request_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    let leases = rows
        .iter()
        .map(|row| decode_store_lease(row).map_err(StoreLeaseError))
        .collect::<Result<Vec<_>, _>>()?;
    if leases
        .iter()
        .filter(|lease| lease.purpose == StoreLeasePurpose::Derivation)
        .count()
        != 1
        || leases.is_empty()
        || leases.iter().any(|lease| {
            lease.owner_kind != StoreLeaseOwnerKind::Request
                || lease.owner_id != request_id
                || lease.state != StoreLeaseState::Active
                || !matches!(
                    lease.purpose,
                    StoreLeasePurpose::Derivation
                        | StoreLeasePurpose::Input
                        | StoreLeasePurpose::Output
                )
        })
    {
        return Err(StoreLeaseError(StoreLeaseFailure::Query));
    }
    Ok(leases)
}

fn release_locked_request_leases(
    transaction: &mut postgres::Transaction<'_>,
    request_id: &str,
    locked: &[StoreLeaseRecord],
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    let releasable = locked
        .iter()
        .filter(|lease| {
            matches!(
                lease.purpose,
                StoreLeasePurpose::Derivation | StoreLeasePurpose::Input
            )
        })
        .collect::<Vec<_>>();
    let rows = transaction
        .query(
            "UPDATE store_leases SET state = 'released', released_at = transaction_timestamp() WHERE owner_kind = 'request' AND owner_id = $1 AND purpose IN ('derivation', 'input') AND state = 'active' RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at",
            &[&request_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    let mut released = rows
        .iter()
        .map(|row| decode_store_lease(row).map_err(StoreLeaseError))
        .collect::<Result<Vec<_>, _>>()?;
    released.sort_by(|left, right| left.lease_id.cmp(&right.lease_id));
    if released.len() != releasable.len()
        || released.iter().zip(releasable).any(|(released, locked)| {
            released.lease_id != locked.lease_id
                || released.owner_kind != locked.owner_kind
                || released.owner_id != locked.owner_id
                || released.store_path != locked.store_path
                || released.purpose != locked.purpose
                || released.state != StoreLeaseState::Released
                || released.created_at != locked.created_at
        })
        || released
            .iter()
            .any(|lease| lease.released_at != released[0].released_at)
    {
        return Err(StoreLeaseError(StoreLeaseFailure::Query));
    }
    Ok(released)
}

fn validate_request_lease_release_inputs(
    database_url: &str,
    session_id: &str,
    request_id: &str,
) -> Result<(), StoreLeaseError> {
    if database_url.trim().is_empty()
        || session_id.is_empty()
        || session_id.len() > MAX_IPC_COMPONENT_BYTES
        || request_id.is_empty()
        || request_id.len() > MAX_IPC_COMPONENT_BYTES
    {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    Ok(())
}

fn emit_request_lease_release_result(
    operation: &'static str,
    path_count: Result<usize, &StoreLeaseError>,
    result: &Result<ReleasedRequestLeases, StoreLeaseError>,
) {
    match result {
        Ok(_) => tracing::info!(
            event = "database.request_lease_release.completed",
            operation,
            owner_kind = "request",
            state = "released",
            path_count = path_count.unwrap_or(0),
            result = "success",
            "request leases released"
        ),
        Err(error) => tracing::warn!(
            event = "database.request_lease_release.failed",
            operation,
            owner_kind = "request",
            state = "released",
            path_count = 0_usize,
            result = "failure",
            failure_class = error.failure().as_str(),
            "request lease release failed"
        ),
    }
}

pub fn release_store_lease(
    database_url: &str,
    lease_id: &str,
) -> Result<StoreLeaseRecord, StoreLeaseError> {
    let result = release_store_lease_inner(database_url, lease_id);
    emit_store_lease_failure("release", &result);
    result
}

fn release_store_lease_inner(
    database_url: &str,
    lease_id: &str,
) -> Result<StoreLeaseRecord, StoreLeaseError> {
    validate_store_lease_id(lease_id)?;
    if database_url.trim().is_empty() {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let existing = transaction
        .query_opt(
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at FROM store_leases WHERE lease_id = $1 FOR UPDATE",
            &[&lease_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match existing {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => match decode_store_lease(&row).map_err(StoreLeaseError)?.state {
            StoreLeaseState::Active => {}
            StoreLeaseState::Released => {
                return Err(StoreLeaseError(StoreLeaseFailure::InvalidState));
            }
        },
    }
    let row = transaction
        .query_one(
            "UPDATE store_leases SET state = 'released', released_at = transaction_timestamp() WHERE lease_id = $1 AND state = 'active' RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at",
            &[&lease_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    let lease = decode_store_lease(&row).map_err(StoreLeaseError)?;
    transaction
        .commit()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Commit))?;
    tracing::info!(
        event = "database.store_lease.released",
        operation = "release",
        owner_kind = lease.owner_kind.as_str(),
        purpose = lease.purpose.as_str(),
        state = "released"
    );
    Ok(lease)
}

const MAXIMUM_RELEASED_REQUEST_LEASES_PAGE_ROWS: usize = 256;

pub fn release_expired_request_output_leases(
    database_url: &str,
    now: SystemTime,
    after_lease_id: Option<&str>,
    maximum_rows: usize,
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    let result = release_expired_request_output_leases_inner(
        database_url,
        now,
        after_lease_id,
        maximum_rows,
    );
    match &result {
        Ok(released) => tracing::info!(
            event = "database.store_lease.expired",
            operation = "expire-output-retention",
            result = "succeeded",
            path_count = released.len(),
        ),
        Err(error) => tracing::warn!(
            event = "database.store_lease.expired",
            operation = "expire-output-retention",
            result = "failed",
            path_count = 0_usize,
            failure_class = error.failure().as_str(),
        ),
    }
    result
}

fn release_expired_request_output_leases_inner(
    database_url: &str,
    now: SystemTime,
    after_lease_id: Option<&str>,
    maximum_rows: usize,
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    if database_url.trim().is_empty()
        || !(1..=256).contains(&maximum_rows)
        || now.duration_since(std::time::UNIX_EPOCH).is_err()
    {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    if let Some(after_lease_id) = after_lease_id {
        validate_store_lease_id(after_lease_id)?;
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let rows = transaction
        .query(
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at FROM store_leases WHERE owner_kind = 'request' AND purpose = 'output' AND state = 'active' AND expires_at <= $1 AND ($2::text IS NULL OR lease_id > $2) ORDER BY lease_id LIMIT $3 FOR UPDATE SKIP LOCKED",
            &[&now, &after_lease_id, &(maximum_rows as i64)],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    let selected = rows
        .iter()
        .map(|row| decode_store_lease(row).map_err(StoreLeaseError))
        .collect::<Result<Vec<_>, _>>()?;
    if selected.iter().any(|lease| {
        lease.owner_kind != StoreLeaseOwnerKind::Request
            || lease.purpose != StoreLeasePurpose::Output
            || lease.state != StoreLeaseState::Active
    }) {
        return Err(StoreLeaseError(StoreLeaseFailure::Query));
    }
    let mut released = Vec::with_capacity(selected.len());
    for lease in &selected {
        let row = transaction
            .query_one(
                "UPDATE store_leases SET state = 'released', released_at = transaction_timestamp() WHERE lease_id = $1 AND state = 'active' RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at",
                &[&lease.lease_id],
            )
            .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
        released.push(decode_store_lease(&row).map_err(StoreLeaseError)?);
    }
    transaction
        .commit()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Commit))?;
    Ok(released)
}

pub fn read_released_request_leases_page(
    database_url: &str,
    after_lease_id: Option<&str>,
    maximum_rows: usize,
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    if database_url.trim().is_empty() {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    if let Some(after_lease_id) = after_lease_id {
        validate_store_lease_id(after_lease_id)?;
    }
    let maximum_rows = maximum_rows.min(MAXIMUM_RELEASED_REQUEST_LEASES_PAGE_ROWS);
    if maximum_rows == 0 {
        return Ok(Vec::new());
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let rows = client
        .query(
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at FROM store_leases WHERE owner_kind = 'request' AND state = 'released' AND ($1::text IS NULL OR lease_id > $1) ORDER BY lease_id LIMIT $2",
            &[&after_lease_id, &(maximum_rows as i64)],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    rows.iter()
        .map(|row| {
            let lease = decode_store_lease(row).map_err(StoreLeaseError)?;
            if lease.owner_kind != StoreLeaseOwnerKind::Request
                || lease.state != StoreLeaseState::Released
            {
                return Err(StoreLeaseError(StoreLeaseFailure::Query));
            }
            Ok(lease)
        })
        .collect()
}

pub fn read_store_lease(
    database_url: &str,
    lease_id: &str,
) -> Result<Option<StoreLeaseRecord>, StoreLeaseError> {
    let result = read_store_lease_inner(database_url, lease_id);
    emit_store_lease_failure("read", &result);
    result
}

fn read_store_lease_inner(
    database_url: &str,
    lease_id: &str,
) -> Result<Option<StoreLeaseRecord>, StoreLeaseError> {
    validate_store_lease_id(lease_id)?;
    if database_url.trim().is_empty() {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let lease = client
        .query_opt(
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at FROM store_leases WHERE lease_id = $1",
            &[&lease_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        .map(|row| decode_store_lease(&row).map_err(StoreLeaseError))
        .transpose()?;
    Ok(lease)
}

fn emit_store_lease_failure<T>(operation: &'static str, result: &Result<T, StoreLeaseError>) {
    if let Err(error) = result {
        tracing::warn!(
            event = "database.store_lease.failed",
            operation,
            failure_class = error.failure().as_str(),
            "store lease persistence failed"
        );
    }
}

fn validate_store_lease_inputs(
    database_url: &str,
    lease_id: &str,
    owner_id: &str,
    store_path: &str,
) -> Result<(), StoreLeaseError> {
    validate_store_lease_id(lease_id)?;
    if database_url.trim().is_empty()
        || owner_id.is_empty()
        || owner_id.len() > MAX_IPC_COMPONENT_BYTES
        || store_path.len() > nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
        || store_path
            .strip_prefix("/nix/store/")
            .is_none_or(str::is_empty)
        || store_path
            .bytes()
            .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r'))
    {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    Ok(())
}

fn validate_store_lease_id(lease_id: &str) -> Result<(), StoreLeaseError> {
    if lease_id.is_empty() || lease_id.len() > MAX_IPC_COMPONENT_BYTES {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    Ok(())
}

fn decode_store_lease(row: &Row) -> Result<StoreLeaseRecord, StoreLeaseFailure> {
    let lease_id: String = row.try_get(0).map_err(|_| StoreLeaseFailure::Query)?;
    let owner_kind: String = row.try_get(1).map_err(|_| StoreLeaseFailure::Query)?;
    let owner_id: String = row.try_get(2).map_err(|_| StoreLeaseFailure::Query)?;
    let store_path: String = row.try_get(3).map_err(|_| StoreLeaseFailure::Query)?;
    let purpose: String = row.try_get(4).map_err(|_| StoreLeaseFailure::Query)?;
    let state: String = row.try_get(5).map_err(|_| StoreLeaseFailure::Query)?;
    let created_at: SystemTime = row.try_get(6).map_err(|_| StoreLeaseFailure::Query)?;
    let released_at: Option<SystemTime> = row.try_get(7).map_err(|_| StoreLeaseFailure::Query)?;
    let expires_at: Option<SystemTime> = row.try_get(8).map_err(|_| StoreLeaseFailure::Query)?;
    validate_store_lease_inputs("validated", &lease_id, &owner_id, &store_path)
        .map_err(|_| StoreLeaseFailure::Query)?;
    let owner_kind = StoreLeaseOwnerKind::parse(&owner_kind).ok_or(StoreLeaseFailure::Query)?;
    let purpose = StoreLeasePurpose::parse(&purpose).ok_or(StoreLeaseFailure::Query)?;
    let state = match StoreLeaseState::parse(&state) {
        Some(StoreLeaseState::Active) if released_at.is_none() => StoreLeaseState::Active,
        Some(StoreLeaseState::Released)
            if released_at.is_some_and(|released_at| released_at >= created_at) =>
        {
            StoreLeaseState::Released
        }
        _ => return Err(StoreLeaseFailure::Query),
    };
    match (purpose, expires_at) {
        (StoreLeasePurpose::Output, None) => return Err(StoreLeaseFailure::Query),
        (StoreLeasePurpose::Output, Some(expires_at))
            if state == StoreLeaseState::Active && expires_at < created_at =>
        {
            return Err(StoreLeaseFailure::Query);
        }
        (purpose, Some(_)) if purpose != StoreLeasePurpose::Output => {
            return Err(StoreLeaseFailure::Query);
        }
        _ => {}
    }
    Ok(StoreLeaseRecord {
        lease_id,
        owner_kind,
        owner_id,
        store_path,
        purpose,
        state,
        created_at,
        released_at,
        expires_at,
    })
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
