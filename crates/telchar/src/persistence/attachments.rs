use super::*;

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
            "SELECT session_id, requester_reference, credential_id, authentication_authority, audit_subject, quota_subject, state, created_at, closed_at FROM protocol_sessions WHERE session_id = $1 FOR NO KEY UPDATE",
            &[&session_id],
        )
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Query))?;
    match session {
        None => return Err(RequestAttachmentError(RequestAttachmentFailure::Missing)),
        Some(row) => match sessions::decode_protocol_session(&row) {
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
    match transaction.query_opt("SELECT request_id, derivation_path, system, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1", &[&request_id])
        .map_err(|_| RequestAttachmentError(RequestAttachmentFailure::Query))?
    {
        None => return Err(RequestAttachmentError(RequestAttachmentFailure::Missing)),
        Some(row) if build_requests::decode_build_request(&row).is_ok() => {}
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

pub(super) fn decode_request_attachment(
    row: &Row,
) -> Result<RequestAttachment, RequestAttachmentFailure> {
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
