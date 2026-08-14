use super::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BuildRequestFailure {
    Configuration,
    Connection,
    Conflict,
    InvalidState,
    Query,
    Commit,
}

impl BuildRequestFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Connection => "connection",
            Self::Conflict => "conflict",
            Self::InvalidState => "invalid_state",
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
            "INSERT INTO build_requests (request_id, derivation_path, system, audit_subject, quota_subject, created_at) VALUES ($1, $2, $3, $4, $5, transaction_timestamp()) RETURNING request_id, derivation_path, system, audit_subject, quota_subject, created_at",
            &[&request_id, &derivation_path, &system, &audit_subject, &quota_subject],
        )
        .map_err(|error| {
            BuildRequestError(if error.as_db_error().is_some_and(|database| {
                database.code() == &postgres::error::SqlState::UNIQUE_VIOLATION
            }) {
                BuildRequestFailure::Conflict
            } else {
                BuildRequestFailure::Query
            })
        })?;
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
            "SELECT request_id, derivation_path, system, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1",
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

pub(super) fn decode_build_request(row: &Row) -> Result<BuildRequestState, BuildRequestFailure> {
    let request = BuildRequestState {
        request_id: row.try_get(0).map_err(|_| BuildRequestFailure::Query)?,
        derivation_path: row.try_get(1).map_err(|_| BuildRequestFailure::Query)?,
        system: row.try_get(2).map_err(|_| BuildRequestFailure::Query)?,
        audit_subject: row.try_get(3).map_err(|_| BuildRequestFailure::Query)?,
        quota_subject: row.try_get(4).map_err(|_| BuildRequestFailure::Query)?,
        created_at: row.try_get(5).map_err(|_| BuildRequestFailure::Query)?,
    };
    validate_build_request_inputs(
        "validated",
        &request.request_id,
        &request.derivation_path,
        &request.system,
        &request.audit_subject,
        &request.quota_subject,
    )
    .map_err(|_| BuildRequestFailure::Query)?;
    Ok(request)
}
