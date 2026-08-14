use super::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LocalBackendExecutionFailure {
    Configuration,
    Connection,
    Conflict,
    InvalidState,
    Query,
    Commit,
}

#[derive(Debug)]
pub struct LocalBackendExecutionError(LocalBackendExecutionFailure);

impl LocalBackendExecutionError {
    pub fn failure(&self) -> LocalBackendExecutionFailure {
        self.0
    }
}

impl fmt::Display for LocalBackendExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("local backend execution registry operation failed")
    }
}

impl std::error::Error for LocalBackendExecutionError {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LocalBackendExecutionState {
    Accepted,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LocalBackendExecution {
    pub backend_execution_id: String,
    pub idempotency_key: String,
    pub specification_digest: [u8; 32],
    pub state: LocalBackendExecutionState,
    pub created_at: SystemTime,
    pub started_at: Option<SystemTime>,
    pub completed_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LocalBackendExecutionResult {
    pub backend_execution_id: String,
    pub classification: String,
    pub result_metadata: serde_json::Value,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CompletedLocalBackendExecution {
    pub execution: LocalBackendExecution,
    pub result: LocalBackendExecutionResult,
}

pub fn register_local_backend_execution(
    database_url: &str,
    backend_execution_id: &str,
    idempotency_key: &str,
    specification_digest: &[u8; 32],
) -> Result<LocalBackendExecution, LocalBackendExecutionError> {
    validate_local_backend_execution_identity(database_url, backend_execution_id, idempotency_key)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Connection))?;
    let existing = transaction
        .query_opt(
            "SELECT backend_execution_id, idempotency_key, specification_digest, state, created_at, started_at, completed_at FROM local_backend_executions WHERE backend_execution_id = $1 OR idempotency_key = $2 FOR UPDATE",
            &[&backend_execution_id, &idempotency_key],
        )
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    if let Some(row) = existing {
        let execution = decode_local_backend_execution(&row)?;
        if execution.backend_execution_id != backend_execution_id
            || execution.idempotency_key != idempotency_key
            || execution.specification_digest != *specification_digest
        {
            return Err(LocalBackendExecutionError(
                LocalBackendExecutionFailure::Conflict,
            ));
        }
        transaction
            .commit()
            .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Commit))?;
        return Ok(execution);
    }
    let row = transaction
        .query_one(
            "INSERT INTO local_backend_executions (backend_execution_id, idempotency_key, specification_digest, state, created_at) VALUES ($1, $2, $3, 'accepted', transaction_timestamp()) RETURNING backend_execution_id, idempotency_key, specification_digest, state, created_at, started_at, completed_at",
            &[&backend_execution_id, &idempotency_key, &&specification_digest[..]],
        )
        .map_err(|error| {
            LocalBackendExecutionError(if error.as_db_error().is_some_and(|database| {
                database.code() == &postgres::error::SqlState::UNIQUE_VIOLATION
            }) {
                LocalBackendExecutionFailure::Conflict
            } else {
                LocalBackendExecutionFailure::Query
            })
        })?;
    let execution = decode_local_backend_execution(&row)?;
    transaction
        .commit()
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Commit))?;
    Ok(execution)
}

pub fn record_local_backend_running(
    database_url: &str,
    backend_execution_id: &str,
) -> Result<LocalBackendExecution, LocalBackendExecutionError> {
    validate_local_backend_execution_identity(database_url, backend_execution_id, "validated")?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Connection))?;
    let row = client
        .query_opt(
            "UPDATE local_backend_executions SET state = 'running', started_at = transaction_timestamp() WHERE backend_execution_id = $1 AND state = 'accepted' AND started_at IS NULL AND completed_at IS NULL RETURNING backend_execution_id, idempotency_key, specification_digest, state, created_at, started_at, completed_at",
            &[&backend_execution_id],
        )
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?
        .ok_or(LocalBackendExecutionError(
            LocalBackendExecutionFailure::InvalidState,
        ))?;
    decode_local_backend_execution(&row)
}

pub fn complete_local_backend_execution(
    database_url: &str,
    backend_execution_id: &str,
    terminal_state: LocalBackendExecutionState,
    classification: &str,
    result_metadata: &serde_json::Value,
) -> Result<CompletedLocalBackendExecution, LocalBackendExecutionError> {
    validate_local_backend_execution_identity(database_url, backend_execution_id, "validated")?;
    let expected_classification = match terminal_state {
        LocalBackendExecutionState::Succeeded => "succeeded",
        LocalBackendExecutionState::Failed => classification,
        LocalBackendExecutionState::Cancelled => "cancelled",
        LocalBackendExecutionState::Accepted | LocalBackendExecutionState::Running => {
            return Err(LocalBackendExecutionError(
                LocalBackendExecutionFailure::Configuration,
            ));
        }
    };
    if classification != expected_classification
        || !matches!(
            classification,
            "succeeded"
                | "build-failure"
                | "infrastructure-failure"
                | "admission-failure"
                | "input-failure"
                | "output-failure"
                | "cancelled"
                | "internal-failure"
        )
    {
        return Err(LocalBackendExecutionError(
            LocalBackendExecutionFailure::Configuration,
        ));
    }
    let result_metadata_text = serde_json::to_string(result_metadata)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Configuration))?;
    if !result_metadata.is_object()
        || result_metadata_text.is_empty()
        || result_metadata_text.len() > MAX_IPC_COMPONENT_BYTES
    {
        return Err(LocalBackendExecutionError(
            LocalBackendExecutionFailure::Configuration,
        ));
    }
    let state = match terminal_state {
        LocalBackendExecutionState::Succeeded => "succeeded",
        LocalBackendExecutionState::Failed => "failed",
        LocalBackendExecutionState::Cancelled => "cancelled",
        LocalBackendExecutionState::Accepted | LocalBackendExecutionState::Running => {
            unreachable!()
        }
    };
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Connection))?;
    let execution_row = transaction
        .query_opt(
            "SELECT backend_execution_id, idempotency_key, specification_digest, state, created_at, started_at, completed_at FROM local_backend_executions WHERE backend_execution_id = $1 FOR UPDATE",
            &[&backend_execution_id],
        )
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?
        .ok_or(LocalBackendExecutionError(
            LocalBackendExecutionFailure::InvalidState,
        ))?;
    let execution = decode_local_backend_execution(&execution_row)?;
    if matches!(
        execution.state,
        LocalBackendExecutionState::Succeeded
            | LocalBackendExecutionState::Failed
            | LocalBackendExecutionState::Cancelled
    ) {
        let result_row = transaction
            .query_opt(
                "SELECT backend_execution_id, classification, result_metadata::text, created_at FROM local_backend_execution_results WHERE backend_execution_id = $1",
                &[&backend_execution_id],
            )
            .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?
            .ok_or(LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
        let result = decode_local_backend_execution_result(&result_row)?;
        if execution.state != terminal_state
            || result.classification != classification
            || result.result_metadata != *result_metadata
        {
            return Err(LocalBackendExecutionError(
                LocalBackendExecutionFailure::Conflict,
            ));
        }
        transaction
            .commit()
            .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Commit))?;
        return Ok(CompletedLocalBackendExecution { execution, result });
    }
    if execution.state != LocalBackendExecutionState::Running {
        return Err(LocalBackendExecutionError(
            LocalBackendExecutionFailure::InvalidState,
        ));
    }
    let result_row = transaction
        .query_one(
            "INSERT INTO local_backend_execution_results (backend_execution_id, classification, result_metadata, created_at) VALUES ($1, $2, $3::text::jsonb, transaction_timestamp()) RETURNING backend_execution_id, classification, result_metadata::text, created_at",
            &[&backend_execution_id, &classification, &result_metadata_text],
        )
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let result = decode_local_backend_execution_result(&result_row)?;
    let execution_row = transaction
        .query_one(
            "UPDATE local_backend_executions SET state = $2, completed_at = transaction_timestamp() WHERE backend_execution_id = $1 AND state = 'running' AND started_at IS NOT NULL AND completed_at IS NULL RETURNING backend_execution_id, idempotency_key, specification_digest, state, created_at, started_at, completed_at",
            &[&backend_execution_id, &state],
        )
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let execution = decode_local_backend_execution(&execution_row)?;
    transaction
        .commit()
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Commit))?;
    Ok(CompletedLocalBackendExecution { execution, result })
}

pub fn read_local_backend_execution_result(
    database_url: &str,
    backend_execution_id: &str,
) -> Result<Option<LocalBackendExecutionResult>, LocalBackendExecutionError> {
    validate_local_backend_execution_identity(database_url, backend_execution_id, "validated")?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Connection))?;
    client
        .query_opt(
            "SELECT backend_execution_id, classification, result_metadata::text, created_at FROM local_backend_execution_results WHERE backend_execution_id = $1",
            &[&backend_execution_id],
        )
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?
        .map(|row| decode_local_backend_execution_result(&row))
        .transpose()
}

pub fn read_local_backend_execution(
    database_url: &str,
    backend_execution_id: &str,
) -> Result<Option<LocalBackendExecution>, LocalBackendExecutionError> {
    validate_local_backend_execution_identity(database_url, backend_execution_id, "validated")?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Connection))?;
    client
        .query_opt(
            "SELECT backend_execution_id, idempotency_key, specification_digest, state, created_at, started_at, completed_at FROM local_backend_executions WHERE backend_execution_id = $1",
            &[&backend_execution_id],
        )
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?
        .map(|row| decode_local_backend_execution(&row))
        .transpose()
}

fn decode_local_backend_execution_result(
    row: &Row,
) -> Result<LocalBackendExecutionResult, LocalBackendExecutionError> {
    decode_local_backend_execution_result_columns(row, 0)
}

fn decode_local_backend_execution_result_columns(
    row: &Row,
    offset: usize,
) -> Result<LocalBackendExecutionResult, LocalBackendExecutionError> {
    let backend_execution_id: String = row
        .try_get(offset)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let classification: String = row
        .try_get(offset + 1)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let result_metadata_text: String = row
        .try_get(offset + 2)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let created_at: SystemTime = row
        .try_get(offset + 3)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    validate_local_backend_execution_identity("validated", &backend_execution_id, "validated")?;
    let result_metadata: serde_json::Value = serde_json::from_str(&result_metadata_text)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    if !result_metadata.is_object()
        || result_metadata_text.is_empty()
        || result_metadata_text.len() > MAX_IPC_COMPONENT_BYTES
    {
        return Err(LocalBackendExecutionError(
            LocalBackendExecutionFailure::Query,
        ));
    }
    Ok(LocalBackendExecutionResult {
        backend_execution_id,
        classification,
        result_metadata,
        created_at,
    })
}

fn validate_local_backend_execution_identity(
    database_url: &str,
    backend_execution_id: &str,
    idempotency_key: &str,
) -> Result<(), LocalBackendExecutionError> {
    if database_url.trim().is_empty()
        || backend_execution_id.is_empty()
        || backend_execution_id.len() > MAX_IPC_COMPONENT_BYTES
        || idempotency_key.is_empty()
        || idempotency_key.len() > MAX_IPC_COMPONENT_BYTES
    {
        return Err(LocalBackendExecutionError(
            LocalBackendExecutionFailure::Configuration,
        ));
    }
    Ok(())
}

fn decode_local_backend_execution(
    row: &Row,
) -> Result<LocalBackendExecution, LocalBackendExecutionError> {
    let backend_execution_id: String = row
        .try_get(0)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let idempotency_key: String = row
        .try_get(1)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let digest: Vec<u8> = row
        .try_get(2)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let state: String = row
        .try_get(3)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let created_at: SystemTime = row
        .try_get(4)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let started_at: Option<SystemTime> = row
        .try_get(5)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let completed_at: Option<SystemTime> = row
        .try_get(6)
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    validate_local_backend_execution_identity(
        "validated",
        &backend_execution_id,
        &idempotency_key,
    )?;
    let specification_digest: [u8; 32] = digest
        .try_into()
        .map_err(|_| LocalBackendExecutionError(LocalBackendExecutionFailure::Query))?;
    let state = match (state.as_str(), started_at, completed_at) {
        ("accepted", None, None) => LocalBackendExecutionState::Accepted,
        ("running", Some(_), None) => LocalBackendExecutionState::Running,
        ("succeeded", _, Some(_)) => LocalBackendExecutionState::Succeeded,
        ("failed", _, Some(_)) => LocalBackendExecutionState::Failed,
        ("cancelled", _, Some(_)) => LocalBackendExecutionState::Cancelled,
        _ => {
            return Err(LocalBackendExecutionError(
                LocalBackendExecutionFailure::Query,
            ));
        }
    };
    Ok(LocalBackendExecution {
        backend_execution_id,
        idempotency_key,
        specification_digest,
        state,
        created_at,
        started_at,
        completed_at,
    })
}
