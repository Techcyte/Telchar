use super::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StoreLeaseFailure {
    Configuration,
    Connection,
    Conflict,
    Capacity,
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
            Self::Capacity => "capacity",
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
    pub nar_size: Option<u64>,
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

pub fn create_request_retained_lease(
    database_url: &str,
    lease_id: &str,
    request_id: &str,
    store_path: &str,
    purpose: StoreLeasePurpose,
    nar_size: u64,
    maximum_retained_bytes: u64,
) -> Result<StoreLeaseRecord, StoreLeaseError> {
    if !matches!(
        purpose,
        StoreLeasePurpose::Derivation | StoreLeasePurpose::Input
    ) {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    let result = create_request_retained_leases_inner(
        database_url,
        request_id,
        maximum_retained_bytes,
        purpose,
        &[(lease_id.to_owned(), store_path.to_owned(), nar_size)],
    )
    .and_then(|leases| {
        leases
            .into_iter()
            .next()
            .ok_or(StoreLeaseError(StoreLeaseFailure::Query))
    });
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
        StoreLeaseOwnerKind::Request => match transaction.query_opt("SELECT request_id, derivation_path, system, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1", &[&owner_id])
            .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        {
            None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
            Some(row) if build_requests::decode_build_request(&row).is_ok() => {}
            Some(_) => return Err(StoreLeaseError(StoreLeaseFailure::Query)),
        },
        StoreLeaseOwnerKind::Session => match transaction
            .query_opt(
                "SELECT session_id, requester_reference, credential_id, authentication_authority, audit_subject, quota_subject, state, created_at, closed_at FROM protocol_sessions WHERE session_id = $1 FOR NO KEY UPDATE",
                &[&owner_id],
            )
            .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        {
            None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
            Some(row) => match sessions::decode_protocol_session(&row) {
                Ok(ProtocolSession {
                    state: ProtocolSessionState::Open,
                    ..
                }) => {}
                Ok(_) => return Err(StoreLeaseError(StoreLeaseFailure::InvalidState)),
                Err(_) => return Err(StoreLeaseError(StoreLeaseFailure::Query)),
            },
        },
    }
    let nar_size = matches!(
        purpose,
        StoreLeasePurpose::Derivation | StoreLeasePurpose::Input
    )
    .then_some(1_i64);
    let row = transaction
        .query_one(
            "INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size) VALUES ($1, $2, $3, $4, $5, 'active', transaction_timestamp(), NULL, CASE WHEN $5 = 'output' THEN transaction_timestamp() + interval '1 hour' ELSE NULL END, $6) RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size",
            &[&lease_id, &owner_kind.as_str(), &owner_id, &store_path, &purpose.as_str(), &nar_size],
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
    let sized = leases
        .iter()
        .map(|(lease_id, store_path)| (lease_id.clone(), store_path.clone(), 1_u64))
        .collect::<Vec<_>>();
    create_request_input_leases_with_limit(database_url, request_id, u64::MAX, &sized)
}

pub fn create_request_input_leases_with_limit(
    database_url: &str,
    request_id: &str,
    maximum_retained_bytes: u64,
    leases: &[(String, String, u64)],
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    if leases.is_empty() {
        return Ok(Vec::new());
    }
    let result = create_request_retained_leases_inner(
        database_url,
        request_id,
        maximum_retained_bytes,
        StoreLeasePurpose::Input,
        leases,
    );
    emit_store_lease_batch_failure(
        result.as_ref().map(|_| ()),
        leases
            .len()
            .min(nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES),
    );
    result
}

fn create_request_retained_leases_inner(
    database_url: &str,
    request_id: &str,
    maximum_retained_bytes: u64,
    purpose: StoreLeasePurpose,
    leases: &[(String, String, u64)],
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    if database_url.trim().is_empty()
        || !matches!(
            purpose,
            StoreLeasePurpose::Derivation | StoreLeasePurpose::Input
        )
        || maximum_retained_bytes == 0
        || request_id.is_empty()
        || request_id.len() > MAX_IPC_COMPONENT_BYTES
        || leases.len() > nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES
    {
        return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
    }
    let mut ids = std::collections::HashSet::new();
    let mut paths = std::collections::HashSet::new();
    for (lease_id, store_path, nar_size) in leases {
        validate_store_lease_id(lease_id)?;
        validate_store_lease_inputs("validated", lease_id, request_id, store_path)?;
        if *nar_size == 0 || *nar_size > i64::MAX as u64 {
            return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
        }
        if !ids.insert(lease_id) || !paths.insert(store_path) {
            return Err(StoreLeaseError(StoreLeaseFailure::Configuration));
        }
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let request = transaction.query_opt("SELECT request_id, derivation_path, system, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1 FOR NO KEY UPDATE", &[&request_id])
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match request {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => {
            build_requests::decode_build_request(&row)
                .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
        }
    }
    transaction
        .query_one(
            "SELECT pg_advisory_xact_lock($1)",
            &[&RETAINED_INPUT_ADMISSION_LOCK_KEY],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    let retained: i64 = transaction
        .query_one(
            "SELECT COALESCE(sum(nar_size), 0)::bigint FROM (SELECT store_path, max(nar_size) AS nar_size FROM store_leases WHERE state = 'active' AND purpose IN ('derivation', 'input') GROUP BY store_path) retained",
            &[],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        .get(0);
    let existing_rows = transaction
        .query(
            "SELECT store_path, nar_size FROM store_leases WHERE state = 'active' AND purpose IN ('derivation', 'input') AND store_path = ANY($1)",
            &[&leases.iter().map(|(_, path, _)| path.as_str()).collect::<Vec<_>>()],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    let existing = existing_rows
        .iter()
        .map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1)))
        .collect::<std::collections::HashMap<_, _>>();
    let additional = leases
        .iter()
        .try_fold(0_u64, |total, (_, path, nar_size)| {
            if let Some(existing_size) = existing.get(path) {
                if *existing_size != *nar_size as i64 {
                    return Err(StoreLeaseError(StoreLeaseFailure::Conflict));
                }
                Ok(total)
            } else {
                total
                    .checked_add(*nar_size)
                    .ok_or(StoreLeaseError(StoreLeaseFailure::Capacity))
            }
        })?;
    if (retained as u64)
        .checked_add(additional)
        .is_none_or(|total| total > maximum_retained_bytes)
    {
        return Err(StoreLeaseError(StoreLeaseFailure::Capacity));
    }
    let mut records = Vec::with_capacity(leases.len());
    for (lease_id, store_path, nar_size) in leases {
        let nar_size = *nar_size as i64;
        let row = transaction
            .query_one(
                "INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size) VALUES ($1, 'request', $2, $3, $4, 'active', transaction_timestamp(), NULL, NULL, $5) RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size",
                &[&lease_id, &request_id, &store_path, &purpose.as_str(), &nar_size],
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

pub fn ensure_request_output_leases(
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
    validate_request_output_lease_inputs(database_url, request_id, leases)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    transaction
        .query_opt(
            "SELECT request_id FROM build_requests WHERE request_id = $1 FOR NO KEY UPDATE",
            &[&request_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?
        .ok_or(StoreLeaseError(StoreLeaseFailure::Missing))?;
    let existing_rows = transaction
        .query(
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size FROM store_leases WHERE owner_kind = 'request' AND owner_id = $1 AND purpose = 'output' ORDER BY lease_id FOR UPDATE",
            &[&request_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    if !existing_rows.is_empty() {
        let records = existing_rows
            .iter()
            .map(decode_store_lease)
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreLeaseError)?;
        if records.len() != leases.len()
            || records.iter().zip(leases).any(|(record, expected)| {
                record.lease_id != expected.0
                    || record.store_path != expected.1
                    || record.state != StoreLeaseState::Active
            })
        {
            return Err(StoreLeaseError(StoreLeaseFailure::Conflict));
        }
        transaction
            .commit()
            .map_err(|_| StoreLeaseError(StoreLeaseFailure::Commit))?;
        return Ok(records);
    }
    let retention_seconds = retention_seconds as f64;
    let mut records = Vec::with_capacity(leases.len());
    for (lease_id, store_path) in leases {
        let row = transaction
            .query_one(
                "INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at) VALUES ($1, 'request', $2, $3, 'output', 'active', transaction_timestamp(), NULL, transaction_timestamp() + make_interval(secs => $4)) RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size",
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

fn validate_request_output_lease_inputs(
    database_url: &str,
    request_id: &str,
    leases: &[(String, String)],
) -> Result<(), StoreLeaseError> {
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
    Ok(())
}

fn create_request_output_leases_inner(
    database_url: &str,
    request_id: &str,
    retention_seconds: u64,
    leases: &[(String, String)],
) -> Result<Vec<StoreLeaseRecord>, StoreLeaseError> {
    validate_request_output_lease_inputs(database_url, request_id, leases)?;
    let retention_seconds = retention_seconds as f64;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Connection))?;
    let request = transaction.query_opt("SELECT request_id, derivation_path, system, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1 FOR NO KEY UPDATE", &[&request_id])
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match request {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => {
            build_requests::decode_build_request(&row)
                .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
        }
    }
    let mut records = Vec::with_capacity(leases.len());
    for (lease_id, store_path) in leases {
        let row = transaction
            .query_one(
                "INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at) VALUES ($1, 'request', $2, $3, 'output', 'active', transaction_timestamp(), NULL, transaction_timestamp() + make_interval(secs => $4)) RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size",
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
    let request = transaction.query_opt("SELECT request_id, derivation_path, system, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1 FOR UPDATE", &[&request_id])
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match request {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => build_requests::decode_build_request(&row)
            .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?,
    };
    let attachment = transaction
        .query_opt(
            "SELECT session_id, request_id, state, attached_at, detached_at, delivered_at FROM request_attachments WHERE session_id = $1 AND request_id = $2 FOR UPDATE",
            &[&session_id, &request_id],
        )
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match attachment {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => match attachments::decode_request_attachment(&row) {
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
    match attachments::decode_request_attachment(&attachment) {
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
    let request = transaction.query_opt("SELECT request_id, derivation_path, system, audit_subject, quota_subject, created_at FROM build_requests WHERE request_id = $1 FOR UPDATE", &[&request_id])
        .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?;
    match request {
        None => return Err(StoreLeaseError(StoreLeaseFailure::Missing)),
        Some(row) => build_requests::decode_build_request(&row)
            .map_err(|_| StoreLeaseError(StoreLeaseFailure::Query))?,
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
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size FROM store_leases WHERE owner_kind = 'request' AND owner_id = $1 ORDER BY lease_id FOR UPDATE",
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
            "UPDATE store_leases SET state = 'released', released_at = transaction_timestamp() WHERE owner_kind = 'request' AND owner_id = $1 AND purpose IN ('derivation', 'input') AND state = 'active' RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size",
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
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size FROM store_leases WHERE lease_id = $1 FOR UPDATE",
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
            "UPDATE store_leases SET state = 'released', released_at = transaction_timestamp() WHERE lease_id = $1 AND state = 'active' RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size",
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
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size FROM store_leases WHERE owner_kind = 'request' AND purpose = 'output' AND state = 'active' AND expires_at <= $1 AND ($2::text IS NULL OR lease_id > $2) ORDER BY lease_id LIMIT $3 FOR UPDATE SKIP LOCKED",
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
                "UPDATE store_leases SET state = 'released', released_at = transaction_timestamp() WHERE lease_id = $1 AND state = 'active' RETURNING lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size",
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
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size FROM store_leases WHERE owner_kind = 'request' AND state = 'released' AND ($1::text IS NULL OR lease_id > $1) ORDER BY lease_id LIMIT $2",
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
            "SELECT lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size FROM store_leases WHERE lease_id = $1",
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
    let nar_size = if row.len() > 9 {
        row.try_get::<_, Option<i64>>(9)
            .map_err(|_| StoreLeaseFailure::Query)?
            .map(|value| u64::try_from(value).map_err(|_| StoreLeaseFailure::Query))
            .transpose()?
    } else {
        None
    };
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
        nar_size,
    })
}
