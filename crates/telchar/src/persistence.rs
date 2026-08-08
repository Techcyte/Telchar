use std::fmt;

use std::time::SystemTime;

use postgres::{Client, NoTls, Row};
use sha2::{Digest, Sha256};

use crate::ipc::{MAX_IPC_COMPONENT_BYTES, RequesterMetadata};

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

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "minimum_lifecycle",
    sql: include_str!("../migrations/0001_minimum_lifecycle.sql"),
}];

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

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BuildRequestState {
    pub request_id: String,
    pub derivation_path: String,
    pub system: String,
    pub created_at: SystemTime,
}

pub fn create_build_request(
    database_url: &str,
    request_id: &str,
    derivation_path: &str,
    system: &str,
) -> Result<BuildRequestState, BuildRequestError> {
    validate_build_request_inputs(database_url, request_id, derivation_path, system)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| BuildRequestError(BuildRequestFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| BuildRequestError(BuildRequestFailure::Connection))?;
    let row = transaction
        .query_one(
            "INSERT INTO build_requests (request_id, derivation_path, system, created_at) VALUES ($1, $2, $3, transaction_timestamp()) RETURNING request_id, derivation_path, system, created_at",
            &[&request_id, &derivation_path, &system],
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
            "SELECT request_id, derivation_path, system, created_at FROM build_requests WHERE request_id = $1",
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
) -> Result<(), BuildRequestError> {
    validate_build_request_id(request_id)?;
    if database_url.trim().is_empty()
        || derivation_path.is_empty()
        || derivation_path.len() > nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
        || system.is_empty()
        || system.len() > MAX_IPC_COMPONENT_BYTES
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
    let created_at: SystemTime = row.try_get(3).map_err(|_| BuildRequestFailure::Query)?;
    validate_build_request_inputs("validated", &request_id, &derivation_path, &system)
        .map_err(|_| BuildRequestFailure::Query)?;
    Ok(BuildRequestState {
        request_id,
        derivation_path,
        system,
        created_at,
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
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RequestAttachment {
    pub session_id: String,
    pub request_id: String,
    pub state: RequestAttachmentState,
    pub attached_at: SystemTime,
    pub detached_at: Option<SystemTime>,
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
            "SELECT session_id, requester_reference, state, created_at, closed_at FROM protocol_sessions WHERE session_id = $1 FOR NO KEY UPDATE",
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
    match transaction
        .query_opt(
            "SELECT request_id, derivation_path, system, created_at FROM build_requests WHERE request_id = $1",
            &[&request_id],
        )
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Query))?
    {
        None => return Err(RequestAttachmentError(RequestAttachmentFailure::Missing)),
        Some(row) if decode_build_request(&row).is_ok() => {}
        Some(_) => return Err(RequestAttachmentError(RequestAttachmentFailure::Query)),
    }
    let row = transaction
        .query_one(
            "INSERT INTO request_attachments (session_id, request_id, state, attached_at, detached_at) VALUES ($1, $2, 'attached', transaction_timestamp(), NULL) RETURNING session_id, request_id, state, attached_at, detached_at",
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
            "UPDATE request_attachments SET state = 'detached', detached_at = transaction_timestamp() WHERE session_id = $1 AND request_id = $2 AND state = 'attached' RETURNING session_id, request_id, state, attached_at, detached_at",
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
            Some(row) if row.try_get::<_, String>(0).ok().as_deref() == Some("detached") => {
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
            "SELECT session_id, request_id, state, attached_at, detached_at FROM request_attachments WHERE session_id = $1 AND request_id = $2",
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
    validate_request_attachment_inputs("validated", &session_id, &request_id)
        .map_err(|_| RequestAttachmentFailure::Query)?;
    let state = match state.as_str() {
        "attached" if detached_at.is_none() => RequestAttachmentState::Attached,
        "detached" if detached_at.is_some_and(|detached_at| detached_at >= attached_at) => {
            RequestAttachmentState::Detached
        }
        _ => return Err(RequestAttachmentFailure::Query),
    };
    Ok(RequestAttachment {
        session_id,
        request_id,
        state,
        attached_at,
        detached_at,
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
    pub state: ProtocolSessionState,
    pub created_at: SystemTime,
    pub closed_at: Option<SystemTime>,
}

pub fn open_protocol_session(
    database_url: &str,
    session_id: &str,
    requester_reference: &str,
) -> Result<ProtocolSession, ProtocolSessionError> {
    validate_protocol_session_inputs(database_url, session_id, requester_reference)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| ProtocolSessionError(ProtocolSessionFailure::Connection))?;
    let row = transaction
        .query_one(
            "INSERT INTO protocol_sessions (session_id, requester_reference, state, created_at, closed_at) VALUES ($1, $2, 'open', transaction_timestamp(), NULL) RETURNING session_id, requester_reference, state, created_at, closed_at",
            &[&session_id, &requester_reference],
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
            "UPDATE protocol_sessions SET state = 'closed', closed_at = transaction_timestamp() WHERE session_id = $1 AND state = 'open' RETURNING session_id, requester_reference, state, created_at, closed_at",
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
            "SELECT session_id, requester_reference, state, created_at, closed_at FROM protocol_sessions WHERE session_id = $1",
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
) -> Result<(), ProtocolSessionError> {
    validate_session_id(session_id)?;
    if database_url.trim().is_empty() || !is_requester_reference(requester_reference) {
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
    let state: String = row.try_get(2).map_err(|_| ProtocolSessionFailure::Query)?;
    let created_at: SystemTime = row.try_get(3).map_err(|_| ProtocolSessionFailure::Query)?;
    let closed_at: Option<SystemTime> =
        row.try_get(4).map_err(|_| ProtocolSessionFailure::Query)?;
    let state = match state.as_str() {
        "open" if closed_at.is_none() && is_requester_reference(&requester_reference) => {
            ProtocolSessionState::Open
        }
        "closed"
            if closed_at.is_some_and(|closed_at| closed_at >= created_at)
                && is_requester_reference(&requester_reference) =>
        {
            ProtocolSessionState::Closed
        }
        _ => return Err(ProtocolSessionFailure::Query),
    };
    Ok(ProtocolSession {
        session_id,
        requester_reference,
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
        StoreLeaseOwnerKind::Request => match transaction
            .query_opt(
                "SELECT request_id, derivation_path, system, created_at FROM build_requests WHERE request_id = $1",
                &[&owner_id],
            )
            .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        {
            None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
            Some(row) if decode_build_request(&row).is_ok() => {}
            Some(_) => return Err(StoreLeaseError(StoreLeaseFailure::Query)),
        },
        StoreLeaseOwnerKind::Session => match transaction
            .query_opt(
                "SELECT session_id, requester_reference, state, created_at, closed_at FROM protocol_sessions WHERE session_id = $1 FOR NO KEY UPDATE",
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
            "INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at) VALUES ($1, $2, $3, $4, $5, 'active', transaction_timestamp(), NULL) RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at",
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
    let request = transaction
        .query_opt(
            "SELECT request_id, derivation_path, system, created_at FROM build_requests WHERE request_id = $1 FOR NO KEY UPDATE",
            &[&request_id],
        )
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
                "INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at) VALUES ($1, 'request', $2, $3, 'input', 'active', transaction_timestamp(), NULL) RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at",
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
    let request = transaction
        .query_opt(
            "SELECT request_id, derivation_path, system, created_at FROM build_requests WHERE request_id = $1 FOR UPDATE",
            &[&request_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match request {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => {
            decode_build_request(&row).map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        }
    };
    let attachment = transaction
        .query_opt(
            "SELECT session_id, request_id, state, attached_at, detached_at FROM request_attachments WHERE session_id = $1 AND request_id = $2 FOR UPDATE",
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
            "UPDATE request_attachments SET state = 'detached', detached_at = transaction_timestamp() WHERE session_id = $1 AND request_id = $2 AND state = 'attached' RETURNING session_id, request_id, state, attached_at, detached_at",
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

fn lock_active_request_leases(
    transaction: &mut postgres::Transaction<'_>,
    request_id: &str,
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    let rows = transaction
        .query(
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at FROM store_leases WHERE owner_kind = 'request' AND owner_id = $1 ORDER BY lease_id FOR UPDATE",
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
                    StoreLeasePurpose::Derivation | StoreLeasePurpose::Input
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
    let rows = transaction
        .query(
            "UPDATE store_leases SET state = 'released', released_at = transaction_timestamp() WHERE owner_kind = 'request' AND owner_id = $1 AND state = 'active' RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at",
            &[&request_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    let mut released = rows
        .iter()
        .map(|row| decode_store_lease(row).map_err(StoreLeaseError))
        .collect::<Result<Vec<_>, _>>()?;
    released.sort_by(|left, right| left.lease_id.cmp(&right.lease_id));
    if released.len() != locked.len()
        || released.iter().zip(locked).any(|(released, locked)| {
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
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at FROM store_leases WHERE lease_id = $1 FOR UPDATE",
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
            "UPDATE store_leases SET state = 'released', released_at = transaction_timestamp() WHERE lease_id = $1 AND state = 'active' RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at",
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
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at FROM store_leases WHERE lease_id = $1",
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
    Ok(StoreLeaseRecord {
        lease_id,
        owner_kind,
        owner_id,
        store_path,
        purpose,
        state,
        created_at,
        released_at,
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
        assert!(
            client
                .query_one("SELECT to_regclass('migration_rollback_proof')::text", &[])
                .expect("table lookup succeeds")
                .get::<_, Option<String>>(0)
                .is_none()
        );
        assert!(
            client
                .query_one("SELECT to_regclass('telchar_schema_migrations')::text", &[])
                .expect("ledger lookup succeeds")
                .get::<_, Option<String>>(0)
                .is_none()
        );
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
