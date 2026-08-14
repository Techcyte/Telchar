use super::*;

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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AuthenticationAuthority {
    OpenSshPublicKey,
    OpenSshCertificate,
}

impl AuthenticationAuthority {
    fn for_credential_id(credential_id: &str) -> Option<Self> {
        if credential_id
            .strip_prefix("ssh-pubkey:")
            .is_some_and(|value| !value.is_empty())
        {
            Some(Self::OpenSshPublicKey)
        } else if credential_id
            .strip_prefix("ssh-cert:")
            .is_some_and(|value| !value.is_empty())
        {
            Some(Self::OpenSshCertificate)
        } else {
            None
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::OpenSshPublicKey => "openssh-public-key",
            Self::OpenSshCertificate => "openssh-certificate",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProtocolSession {
    pub session_id: String,
    pub requester_reference: String,
    pub credential_id: Option<String>,
    pub authentication_authority: Option<AuthenticationAuthority>,
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
    credential_id: &str,
    audit_subject: &str,
    quota_subject: &str,
) -> Result<ProtocolSession, ProtocolSessionError> {
    let authentication_authority = validate_protocol_session_inputs(
        database_url,
        session_id,
        requester_reference,
        credential_id,
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
            "INSERT INTO protocol_sessions (session_id, requester_reference, credential_id, authentication_authority, audit_subject, quota_subject, state, created_at, closed_at) VALUES ($1, $2, $3, $4, $5, $6, 'open', transaction_timestamp(), NULL) RETURNING session_id, requester_reference, credential_id, authentication_authority, audit_subject, quota_subject, state, created_at, closed_at",
            &[&session_id, &requester_reference, &credential_id, &authentication_authority.as_str(), &audit_subject, &quota_subject],
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
            "UPDATE protocol_sessions SET state = 'closed', closed_at = transaction_timestamp() WHERE session_id = $1 AND state = 'open' RETURNING session_id, requester_reference, credential_id, authentication_authority, audit_subject, quota_subject, state, created_at, closed_at",
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
            "SELECT session_id, requester_reference, credential_id, authentication_authority, audit_subject, quota_subject, state, created_at, closed_at FROM protocol_sessions WHERE session_id = $1",
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
    credential_id: &str,
    audit_subject: &str,
    quota_subject: &str,
) -> Result<AuthenticationAuthority, ProtocolSessionError> {
    validate_session_id(session_id)?;
    let authentication_authority = AuthenticationAuthority::for_credential_id(credential_id)
        .ok_or(ProtocolSessionError(ProtocolSessionFailure::Configuration))?;
    if database_url.trim().is_empty()
        || !is_requester_reference(requester_reference)
        || credential_id.len() > crate::service::ipc::MAX_IPC_CREDENTIAL_ID_BYTES
        || audit_subject.is_empty()
        || audit_subject.len() > MAX_IPC_COMPONENT_BYTES
        || quota_subject.is_empty()
        || quota_subject.len() > crate::service::ipc::MAX_IPC_CREDENTIAL_ID_BYTES
    {
        return Err(ProtocolSessionError(ProtocolSessionFailure::Configuration));
    }
    Ok(authentication_authority)
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

pub(super) fn decode_protocol_session(
    row: &Row,
) -> Result<ProtocolSession, ProtocolSessionFailure> {
    let session_id: String = row.try_get(0).map_err(|_| ProtocolSessionFailure::Query)?;
    let requester_reference: String = row.try_get(1).map_err(|_| ProtocolSessionFailure::Query)?;
    let credential_id: Option<String> =
        row.try_get(2).map_err(|_| ProtocolSessionFailure::Query)?;
    let authentication_authority: Option<String> =
        row.try_get(3).map_err(|_| ProtocolSessionFailure::Query)?;
    let audit_subject: String = row.try_get(4).map_err(|_| ProtocolSessionFailure::Query)?;
    let quota_subject: String = row.try_get(5).map_err(|_| ProtocolSessionFailure::Query)?;
    let state: String = row.try_get(6).map_err(|_| ProtocolSessionFailure::Query)?;
    let created_at: SystemTime = row.try_get(7).map_err(|_| ProtocolSessionFailure::Query)?;
    let closed_at: Option<SystemTime> =
        row.try_get(8).map_err(|_| ProtocolSessionFailure::Query)?;
    let authentication_authority = match (
        credential_id.as_deref(),
        authentication_authority.as_deref(),
    ) {
        (None, None) => None,
        (Some(credential_id), Some(authority)) => {
            let decoded = AuthenticationAuthority::for_credential_id(credential_id)
                .ok_or(ProtocolSessionFailure::Query)?;
            if decoded.as_str() != authority {
                return Err(ProtocolSessionFailure::Query);
            }
            Some(decoded)
        }
        _ => return Err(ProtocolSessionFailure::Query),
    };
    let state = match state.as_str() {
        "open"
            if closed_at.is_none()
                && is_requester_reference(&requester_reference)
                && !audit_subject.is_empty()
                && audit_subject.len() <= MAX_IPC_COMPONENT_BYTES
                && !quota_subject.is_empty()
                && quota_subject.len() <= crate::service::ipc::MAX_IPC_CREDENTIAL_ID_BYTES =>
        {
            ProtocolSessionState::Open
        }
        "closed"
            if closed_at.is_some_and(|closed_at| closed_at >= created_at)
                && is_requester_reference(&requester_reference)
                && !audit_subject.is_empty()
                && audit_subject.len() <= MAX_IPC_COMPONENT_BYTES
                && !quota_subject.is_empty()
                && quota_subject.len() <= crate::service::ipc::MAX_IPC_CREDENTIAL_ID_BYTES =>
        {
            ProtocolSessionState::Closed
        }
        _ => return Err(ProtocolSessionFailure::Query),
    };
    Ok(ProtocolSession {
        session_id,
        requester_reference,
        credential_id,
        authentication_authority,
        audit_subject,
        quota_subject,
        state,
        created_at,
        closed_at,
    })
}
