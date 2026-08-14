use super::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SharedBuildFailure {
    Configuration,
    Connection,
    Conflict,
    InvalidState,
    Quota,
    Query,
    Commit,
}

impl SharedBuildFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Connection => "connection",
            Self::Conflict => "conflict",
            Self::InvalidState => "invalid_state",
            Self::Quota => "quota",
            Self::Query => "query",
            Self::Commit => "commit",
        }
    }
}

#[derive(Debug)]
pub struct SharedBuildError(SharedBuildFailure);

impl SharedBuildError {
    pub fn failure(&self) -> SharedBuildFailure {
        self.0
    }
}

impl fmt::Display for SharedBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("shared build state operation failed")
    }
}

impl std::error::Error for SharedBuildError {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SharedBuildState {
    Claimed,
    Running,
    Collecting,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SharedBuildOwnership {
    Claimed,
    Joined,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SharedBuild {
    pub derivation_path: String,
    pub request_digest: [u8; 32],
    pub state: SharedBuildState,
    pub backend_name: String,
    pub backend_kind: BackendKind,
    pub capabilities: BackendCapabilities,
    pub backend_execution_id: Option<String>,
    pub expected_outputs: Vec<String>,
    pub build_request: Option<crate::build::BuildRequest>,
    pub result_metadata: Option<serde_json::Value>,
    pub failure_classification: Option<String>,
    pub created_at: SystemTime,
    pub started_at: Option<SystemTime>,
    pub collecting_at: Option<SystemTime>,
    pub completed_at: Option<SystemTime>,
    pub expires_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SharedBuildClaim {
    pub ownership: SharedBuildOwnership,
    pub build: SharedBuild,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SharedBuildQueueEntry {
    pub derivation_path: String,
    pub quota_subject: String,
    pub queue_position: i64,
    pub queued_at: SystemTime,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SharedBuildOperationalCounts {
    pub queued: u64,
    pub running: u64,
    pub collecting: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SharedBuildAttemptState {
    Running,
    Collecting,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SharedBuildAttempt {
    pub attempt_id: i64,
    pub derivation_path: String,
    pub ordinal: i32,
    pub backend_name: String,
    pub backend_kind: BackendKind,
    pub backend_execution_id: Option<String>,
    pub state: SharedBuildAttemptState,
    pub created_at: SystemTime,
    pub started_at: Option<SystemTime>,
    pub collecting_at: Option<SystemTime>,
    pub completed_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SharedBuildAttemptOutcome {
    pub attempt_id: i64,
    pub classification: String,
    pub result_metadata: serde_json::Value,
    pub created_at: SystemTime,
}

pub fn enqueue_shared_build(
    database_url: &str,
    derivation_path: &str,
    quota_subject: &str,
    maximum_queued_builds: usize,
) -> Result<SharedBuildQueueEntry, SharedBuildError> {
    validate_shared_build_identity(database_url, derivation_path)?;
    if quota_subject.is_empty()
        || quota_subject.len() > crate::service::ipc::MAX_IPC_CREDENTIAL_ID_BYTES
        || quota_subject.contains('\0')
        || maximum_queued_builds == 0
        || maximum_queued_builds > 65_536
    {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    let maximum_queued_builds = i64::try_from(maximum_queued_builds)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Configuration))?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    transaction
        .query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            &[&quota_subject],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    let queued_count: i64 = transaction
        .query_one(
            "SELECT count(*) FROM shared_builds
             WHERE state = 'claimed' AND quota_subject = $1",
            &[&quota_subject],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .try_get(0)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    if queued_count >= maximum_queued_builds {
        return Err(SharedBuildError(SharedBuildFailure::Quota));
    }
    let row = transaction
        .query_opt(
            "UPDATE shared_builds
             SET quota_subject = $2,
                 queue_position = nextval('shared_build_queue_position_seq'),
                 queued_at = transaction_timestamp()
             WHERE derivation_path = $1
               AND state = 'claimed'
               AND quota_subject IS NULL
             RETURNING derivation_path, quota_subject, queue_position, queued_at",
            &[&derivation_path, &quota_subject],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .ok_or(SharedBuildError(SharedBuildFailure::InvalidState))?;
    let entry = decode_shared_build_queue_entry(&row).map_err(SharedBuildError)?;
    transaction
        .commit()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Commit))?;
    Ok(entry)
}

pub fn start_queued_shared_build(
    database_url: &str,
    derivation_path: &str,
    maximum_active_builds: usize,
) -> Result<SharedBuild, SharedBuildError> {
    validate_shared_build_identity(database_url, derivation_path)?;
    if maximum_active_builds == 0 || maximum_active_builds > 65_536 {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    let maximum_active_builds = i64::try_from(maximum_active_builds)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Configuration))?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let quota_subject: String = transaction
        .query_opt(
            "SELECT quota_subject FROM shared_builds
             WHERE derivation_path = $1 AND state = 'claimed' AND quota_subject IS NOT NULL
             FOR UPDATE",
            &[&derivation_path],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .ok_or(SharedBuildError(SharedBuildFailure::InvalidState))?
        .try_get(0)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    transaction
        .query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            &[&quota_subject],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    let active_count: i64 = transaction
        .query_one(
            "SELECT count(*) FROM shared_builds
             WHERE quota_subject = $1 AND state IN ('running', 'collecting')",
            &[&quota_subject],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .try_get(0)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    if active_count >= maximum_active_builds {
        return Err(SharedBuildError(SharedBuildFailure::Quota));
    }
    let row = transaction
        .query_one(
            "UPDATE shared_builds
             SET state = 'running', started_at = transaction_timestamp()
             WHERE derivation_path = $1 AND state = 'claimed'
             RETURNING derivation_path, request_digest, state, backend_name, backend_kind,
                       execution_recovery, cancellation, log_recovery,
                       backend_execution_id, expected_outputs, build_request::text, result_metadata::text,
                       failure_classification, created_at, started_at, collecting_at,
                       completed_at, expires_at",
            &[&derivation_path],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    let build = decode_shared_build(&row).map_err(SharedBuildError)?;
    create_shared_build_attempt(&mut transaction, &build)?;
    transaction
        .commit()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Commit))?;
    Ok(build)
}

pub fn read_queued_shared_builds(
    database_url: &str,
    limit: usize,
) -> Result<Vec<SharedBuildQueueEntry>, SharedBuildError> {
    if database_url.trim().is_empty() || limit == 0 || limit > 256 {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    let limit =
        i64::try_from(limit).map_err(|_| SharedBuildError(SharedBuildFailure::Configuration))?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    client
        .query(
            "SELECT derivation_path, quota_subject, queue_position, queued_at
             FROM shared_builds
             WHERE state = 'claimed' AND quota_subject IS NOT NULL
             ORDER BY queue_position
             LIMIT $1",
            &[&limit],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .into_iter()
        .map(|row| decode_shared_build_queue_entry(&row).map_err(SharedBuildError))
        .collect()
}

pub fn read_shared_build_scheduler_subject(
    database_url: &str,
) -> Result<Option<String>, SharedBuildError> {
    if database_url.trim().is_empty() {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    client
        .query_one(
            "SELECT last_admitted_subject FROM shared_build_scheduler_state WHERE singleton",
            &[],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .try_get(0)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))
}

pub fn record_shared_build_scheduler_subject(
    database_url: &str,
    quota_subject: &str,
) -> Result<(), SharedBuildError> {
    if database_url.trim().is_empty()
        || quota_subject.is_empty()
        || quota_subject.len() > crate::service::ipc::MAX_IPC_CREDENTIAL_ID_BYTES
        || quota_subject.contains('\0')
    {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    client
        .execute(
            "UPDATE shared_build_scheduler_state
             SET last_admitted_subject = $1, updated_at = transaction_timestamp()
             WHERE singleton",
            &[&quota_subject],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    Ok(())
}

pub fn read_next_queued_shared_build(
    database_url: &str,
    after_quota_subject: Option<&str>,
    maximum_subjects: usize,
) -> Result<Option<SharedBuildQueueEntry>, SharedBuildError> {
    if database_url.trim().is_empty()
        || maximum_subjects == 0
        || maximum_subjects > 256
        || after_quota_subject.is_some_and(|subject| {
            subject.is_empty()
                || subject.len() > crate::service::ipc::MAX_IPC_CREDENTIAL_ID_BYTES
                || subject.contains('\0')
        })
    {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    let maximum_subjects = i64::try_from(maximum_subjects)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Configuration))?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let rows = client
        .query(
            "SELECT DISTINCT ON (quota_subject)
                    derivation_path, quota_subject, queue_position, queued_at
             FROM shared_builds
             WHERE state = 'claimed' AND quota_subject IS NOT NULL
             ORDER BY quota_subject, queue_position
             LIMIT $1",
            &[&maximum_subjects],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    let mut entries = rows
        .into_iter()
        .map(|row| decode_shared_build_queue_entry(&row).map_err(SharedBuildError))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.quota_subject.cmp(&right.quota_subject));
    let selected = after_quota_subject
        .and_then(|subject| {
            entries
                .iter()
                .position(|entry| entry.quota_subject.as_str() > subject)
        })
        .or_else(|| (!entries.is_empty()).then_some(0));
    Ok(selected.map(|index| entries.swap_remove(index)))
}

fn decode_shared_build_queue_entry(row: &Row) -> Result<SharedBuildQueueEntry, SharedBuildFailure> {
    let derivation_path: String = row.try_get(0).map_err(|_| SharedBuildFailure::Query)?;
    let quota_subject: String = row.try_get(1).map_err(|_| SharedBuildFailure::Query)?;
    let queue_position: i64 = row.try_get(2).map_err(|_| SharedBuildFailure::Query)?;
    let queued_at: SystemTime = row.try_get(3).map_err(|_| SharedBuildFailure::Query)?;
    if derivation_path.is_empty() || quota_subject.is_empty() || queue_position <= 0 {
        return Err(SharedBuildFailure::Query);
    }
    Ok(SharedBuildQueueEntry {
        derivation_path,
        quota_subject,
        queue_position,
        queued_at,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn claim_shared_build(
    database_url: &str,
    derivation_path: &str,
    request_digest: &[u8],
    backend_name: &str,
    backend_kind: BackendKind,
    capabilities: BackendCapabilities,
    backend_execution_id: Option<&str>,
    expected_outputs: &[&str],
) -> Result<SharedBuildClaim, SharedBuildError> {
    claim_shared_build_inner(
        database_url,
        derivation_path,
        request_digest,
        backend_name,
        backend_kind,
        capabilities,
        backend_execution_id,
        expected_outputs,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn claim_shared_build_with_request(
    database_url: &str,
    derivation_path: &str,
    request_digest: &[u8],
    backend_name: &str,
    backend_kind: BackendKind,
    capabilities: BackendCapabilities,
    backend_execution_id: Option<&str>,
    expected_outputs: &[&str],
    build_request: &crate::build::BuildRequest,
) -> Result<SharedBuildClaim, SharedBuildError> {
    if build_request.validate_for_execution().is_err()
        || build_request.derivation_path() != derivation_path.as_bytes()
        || build_request.shared_build_digest().as_slice() != request_digest
        || build_request
            .expected_outputs()
            .iter()
            .map(|(_, path)| path.as_slice())
            .ne(expected_outputs.iter().map(|path| path.as_bytes()))
    {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    claim_shared_build_inner(
        database_url,
        derivation_path,
        request_digest,
        backend_name,
        backend_kind,
        capabilities,
        backend_execution_id,
        expected_outputs,
        Some(build_request),
    )
}

#[allow(clippy::too_many_arguments)]
fn claim_shared_build_inner(
    database_url: &str,
    derivation_path: &str,
    request_digest: &[u8],
    backend_name: &str,
    backend_kind: BackendKind,
    capabilities: BackendCapabilities,
    backend_execution_id: Option<&str>,
    expected_outputs: &[&str],
    build_request: Option<&crate::build::BuildRequest>,
) -> Result<SharedBuildClaim, SharedBuildError> {
    validate_shared_build_claim(
        database_url,
        derivation_path,
        request_digest,
        backend_name,
        backend_kind,
        capabilities,
        backend_execution_id,
        expected_outputs,
    )?;
    let encoded_build_request = build_request
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Configuration))?;
    let backend_kind_value = backend_kind_name(backend_kind);
    let execution_recovery = execution_recovery_name(capabilities.execution_recovery());
    let cancellation = cancellation_name(capabilities.cancellation());
    let log_recovery = log_recovery_name(capabilities.log_recovery());
    let expected_outputs = expected_outputs
        .iter()
        .map(|output| (*output).to_owned())
        .collect::<Vec<_>>();
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let inserted = transaction
        .query_opt(
            "INSERT INTO shared_builds (
                 derivation_path, request_digest, backend_name, backend_kind,
                 execution_recovery, cancellation, log_recovery,
                 backend_execution_id, expected_outputs, build_request
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::text::jsonb)
             ON CONFLICT (derivation_path) DO UPDATE
             SET request_digest = EXCLUDED.request_digest,
                 state = 'claimed',
                 backend_name = EXCLUDED.backend_name,
                 backend_kind = EXCLUDED.backend_kind,
                 execution_recovery = EXCLUDED.execution_recovery,
                 cancellation = EXCLUDED.cancellation,
                 log_recovery = EXCLUDED.log_recovery,
                 backend_execution_id = EXCLUDED.backend_execution_id,
                 expected_outputs = EXCLUDED.expected_outputs,
                 build_request = EXCLUDED.build_request,
                 result_metadata = NULL,
                 failure_classification = NULL,
                 created_at = transaction_timestamp(),
                 started_at = NULL,
                 collecting_at = NULL,
                 completed_at = NULL,
                 expires_at = NULL
             WHERE shared_builds.state = 'failed'
             RETURNING derivation_path",
            &[
                &derivation_path,
                &request_digest,
                &backend_name,
                &backend_kind_value,
                &execution_recovery,
                &cancellation,
                &log_recovery,
                &backend_execution_id,
                &expected_outputs,
                &encoded_build_request,
            ],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .is_some();
    let row = transaction
        .query_one(
            "SELECT derivation_path, request_digest, state, backend_name, backend_kind,
                    execution_recovery, cancellation, log_recovery,
                    backend_execution_id, expected_outputs, build_request::text, result_metadata::text,
                    failure_classification, created_at, started_at, collecting_at,
                    completed_at, expires_at
             FROM shared_builds WHERE derivation_path = $1",
            &[&derivation_path],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    let build = decode_shared_build(&row).map_err(SharedBuildError)?;
    if build.request_digest.as_slice() != request_digest {
        return Err(SharedBuildError(SharedBuildFailure::Conflict));
    }
    if !inserted
        && (build.backend_name != backend_name
            || build.backend_kind != backend_kind
            || build.capabilities != capabilities
            || build.backend_execution_id.as_deref() != backend_execution_id
            || build.expected_outputs != expected_outputs
            || build.build_request.as_ref() != build_request)
    {
        return Err(SharedBuildError(SharedBuildFailure::Conflict));
    }
    transaction
        .commit()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Commit))?;
    Ok(SharedBuildClaim {
        ownership: if inserted {
            SharedBuildOwnership::Claimed
        } else {
            SharedBuildOwnership::Joined
        },
        build,
    })
}

pub fn read_shared_build_by_execution(
    database_url: &str,
    backend_name: &str,
    backend_execution_id: &str,
) -> Result<Option<SharedBuild>, SharedBuildError> {
    if database_url.trim().is_empty()
        || backend_name.is_empty()
        || backend_name.len() > MAX_IPC_COMPONENT_BYTES
        || backend_name.contains('\0')
        || backend_execution_id.is_empty()
        || backend_execution_id.len() > nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
        || backend_execution_id.contains('\0')
    {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    client
        .query_opt(
            "SELECT derivation_path, request_digest, state, backend_name, backend_kind,
                    execution_recovery, cancellation, log_recovery,
                    backend_execution_id, expected_outputs, build_request::text, result_metadata::text,
                    failure_classification, created_at, started_at, collecting_at,
                    completed_at, expires_at
             FROM shared_builds
             WHERE backend_name = $1
               AND backend_execution_id = $2
               AND state IN ('running', 'collecting')",
            &[&backend_name, &backend_execution_id],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .map(|row| decode_shared_build(&row).map_err(SharedBuildError))
        .transpose()
}

pub fn read_shared_build(
    database_url: &str,
    derivation_path: &str,
) -> Result<Option<SharedBuild>, SharedBuildError> {
    validate_shared_build_identity(database_url, derivation_path)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    client
        .query_opt(
            "SELECT derivation_path, request_digest, state, backend_name, backend_kind,
                    execution_recovery, cancellation, log_recovery,
                    backend_execution_id, expected_outputs, build_request::text, result_metadata::text,
                    failure_classification, created_at, started_at, collecting_at,
                    completed_at, expires_at
             FROM shared_builds WHERE derivation_path = $1",
            &[&derivation_path],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .map(|row| decode_shared_build(&row).map_err(SharedBuildError))
        .transpose()
}

pub fn read_shared_build_operational_counts(
    database_url: &str,
) -> Result<SharedBuildOperationalCounts, SharedBuildError> {
    if database_url.trim().is_empty() {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let row = client
        .query_one(
            "SELECT count(*) FILTER (WHERE state = 'claimed' AND queue_position IS NOT NULL),
                    count(*) FILTER (WHERE state = 'running'),
                    count(*) FILTER (WHERE state = 'collecting')
             FROM shared_builds",
            &[],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    let count = |index| -> Result<u64, SharedBuildError> {
        u64::try_from(
            row.try_get::<_, i64>(index)
                .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?,
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))
    };
    Ok(SharedBuildOperationalCounts {
        queued: count(0)?,
        running: count(1)?,
        collecting: count(2)?,
    })
}

pub fn read_active_shared_builds(
    database_url: &str,
    limit: usize,
) -> Result<Vec<SharedBuild>, SharedBuildError> {
    if database_url.trim().is_empty() || limit == 0 || limit > 256 {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    let limit =
        i64::try_from(limit).map_err(|_| SharedBuildError(SharedBuildFailure::Configuration))?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    client
        .query(
            "SELECT derivation_path, request_digest, state, backend_name, backend_kind,
                    execution_recovery, cancellation, log_recovery,
                    backend_execution_id, expected_outputs, build_request::text, result_metadata::text,
                    failure_classification, created_at, started_at, collecting_at,
                    completed_at, expires_at
             FROM shared_builds
             WHERE state IN ('claimed', 'running', 'collecting')
             ORDER BY created_at, derivation_path
             LIMIT $1",
            &[&limit],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .into_iter()
        .map(|row| decode_shared_build(&row).map_err(SharedBuildError))
        .collect()
}

pub fn start_shared_build(
    database_url: &str,
    derivation_path: &str,
) -> Result<SharedBuild, SharedBuildError> {
    validate_shared_build_identity(database_url, derivation_path)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let row = transaction
        .query_opt(
            "UPDATE shared_builds
             SET state = 'running', started_at = transaction_timestamp()
             WHERE derivation_path = $1 AND state = 'claimed'
             RETURNING derivation_path, request_digest, state, backend_name, backend_kind,
                       execution_recovery, cancellation, log_recovery,
                       backend_execution_id, expected_outputs, build_request::text, result_metadata::text,
                       failure_classification, created_at, started_at, collecting_at,
                       completed_at, expires_at",
            &[&derivation_path],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .ok_or(SharedBuildError(SharedBuildFailure::InvalidState))?;
    let build = decode_shared_build(&row).map_err(SharedBuildError)?;
    create_shared_build_attempt(&mut transaction, &build)?;
    transaction
        .commit()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Commit))?;
    Ok(build)
}

pub fn collect_shared_build(
    database_url: &str,
    derivation_path: &str,
) -> Result<SharedBuild, SharedBuildError> {
    validate_shared_build_identity(database_url, derivation_path)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let row = transaction
        .query_opt(
            "UPDATE shared_builds
             SET state = 'collecting', collecting_at = transaction_timestamp()
             WHERE derivation_path = $1 AND state = 'running'
             RETURNING derivation_path, request_digest, state, backend_name, backend_kind,
                       execution_recovery, cancellation, log_recovery,
                       backend_execution_id, expected_outputs, build_request::text, result_metadata::text,
                       failure_classification, created_at, started_at, collecting_at,
                       completed_at, expires_at",
            &[&derivation_path],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .ok_or(SharedBuildError(SharedBuildFailure::InvalidState))?;
    transaction
        .execute(
            "UPDATE shared_build_attempts
             SET state = 'collecting', collecting_at = transaction_timestamp()
             WHERE derivation_path = $1 AND state = 'running'",
            &[&derivation_path],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .eq(&1)
        .then_some(())
        .ok_or(SharedBuildError(SharedBuildFailure::InvalidState))?;
    let build = decode_shared_build(&row).map_err(SharedBuildError)?;
    transaction
        .commit()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Commit))?;
    Ok(build)
}

pub fn complete_shared_build_success(
    database_url: &str,
    derivation_path: &str,
    result_metadata: &serde_json::Value,
    retention: Duration,
) -> Result<SharedBuild, SharedBuildError> {
    complete_shared_build(
        database_url,
        derivation_path,
        SharedBuildState::Succeeded,
        None,
        result_metadata,
        retention,
    )
}

pub fn complete_shared_build_failure(
    database_url: &str,
    derivation_path: &str,
    failure_classification: &str,
    result_metadata: &serde_json::Value,
    retention: Duration,
) -> Result<SharedBuild, SharedBuildError> {
    complete_shared_build(
        database_url,
        derivation_path,
        SharedBuildState::Failed,
        Some(failure_classification),
        result_metadata,
        retention,
    )
}

fn create_shared_build_attempt(
    transaction: &mut postgres::Transaction<'_>,
    build: &SharedBuild,
) -> Result<SharedBuildAttempt, SharedBuildError> {
    let ordinal: i32 = transaction
        .query_one(
            "SELECT COALESCE(max(ordinal), 0) + 1 FROM shared_build_attempts WHERE derivation_path = $1",
            &[&build.derivation_path],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .try_get(0)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    let row = transaction
        .query_one(
            "INSERT INTO shared_build_attempts (
                 derivation_path, ordinal, backend_name, backend_kind,
                 backend_execution_id, state, started_at
             )
             VALUES ($1, $2, $3, $4, $5, 'running', transaction_timestamp())
             RETURNING attempt_id, derivation_path, ordinal, backend_name, backend_kind,
                       backend_execution_id, state, created_at, started_at, collecting_at,
                       completed_at",
            &[
                &build.derivation_path,
                &ordinal,
                &build.backend_name,
                &backend_kind_name(build.backend_kind),
                &build.backend_execution_id,
            ],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    decode_shared_build_attempt(&row).map_err(SharedBuildError)
}

pub fn read_shared_build_attempt(
    database_url: &str,
    derivation_path: &str,
) -> Result<Option<SharedBuildAttempt>, SharedBuildError> {
    validate_shared_build_identity(database_url, derivation_path)?;
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    client
        .query_opt(
            "SELECT attempt_id, derivation_path, ordinal, backend_name, backend_kind,
                    backend_execution_id, state, created_at, started_at, collecting_at,
                    completed_at
             FROM shared_build_attempts
             WHERE derivation_path = $1
             ORDER BY ordinal DESC
             LIMIT 1",
            &[&derivation_path],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .map(|row| decode_shared_build_attempt(&row).map_err(SharedBuildError))
        .transpose()
}

pub fn read_shared_build_attempt_outcome(
    database_url: &str,
    attempt_id: &i64,
) -> Result<Option<SharedBuildAttemptOutcome>, SharedBuildError> {
    if database_url.trim().is_empty() || *attempt_id <= 0 {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    client
        .query_opt(
            "SELECT attempt_id, classification, result_metadata::text, created_at
             FROM shared_build_attempt_outcomes WHERE attempt_id = $1",
            &[attempt_id],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .map(|row| decode_shared_build_attempt_outcome(&row).map_err(SharedBuildError))
        .transpose()
}

fn decode_shared_build_attempt(row: &Row) -> Result<SharedBuildAttempt, SharedBuildFailure> {
    let backend_kind: String = row.try_get(4).map_err(|_| SharedBuildFailure::Query)?;
    let state: String = row.try_get(6).map_err(|_| SharedBuildFailure::Query)?;
    Ok(SharedBuildAttempt {
        attempt_id: row.try_get(0).map_err(|_| SharedBuildFailure::Query)?,
        derivation_path: row.try_get(1).map_err(|_| SharedBuildFailure::Query)?,
        ordinal: row.try_get(2).map_err(|_| SharedBuildFailure::Query)?,
        backend_name: row.try_get(3).map_err(|_| SharedBuildFailure::Query)?,
        backend_kind: parse_backend_kind(&backend_kind).ok_or(SharedBuildFailure::Query)?,
        backend_execution_id: row.try_get(5).map_err(|_| SharedBuildFailure::Query)?,
        state: match state.as_str() {
            "running" => SharedBuildAttemptState::Running,
            "collecting" => SharedBuildAttemptState::Collecting,
            "succeeded" => SharedBuildAttemptState::Succeeded,
            "failed" => SharedBuildAttemptState::Failed,
            _ => return Err(SharedBuildFailure::Query),
        },
        created_at: row.try_get(7).map_err(|_| SharedBuildFailure::Query)?,
        started_at: row.try_get(8).map_err(|_| SharedBuildFailure::Query)?,
        collecting_at: row.try_get(9).map_err(|_| SharedBuildFailure::Query)?,
        completed_at: row.try_get(10).map_err(|_| SharedBuildFailure::Query)?,
    })
}

fn decode_shared_build_attempt_outcome(
    row: &Row,
) -> Result<SharedBuildAttemptOutcome, SharedBuildFailure> {
    let result_metadata: String = row.try_get(2).map_err(|_| SharedBuildFailure::Query)?;
    let result_metadata: serde_json::Value =
        serde_json::from_str(&result_metadata).map_err(|_| SharedBuildFailure::Query)?;
    if !result_metadata.is_object() {
        return Err(SharedBuildFailure::Query);
    }
    Ok(SharedBuildAttemptOutcome {
        attempt_id: row.try_get(0).map_err(|_| SharedBuildFailure::Query)?,
        classification: row.try_get(1).map_err(|_| SharedBuildFailure::Query)?,
        result_metadata,
        created_at: row.try_get(3).map_err(|_| SharedBuildFailure::Query)?,
    })
}

fn complete_shared_build(
    database_url: &str,
    derivation_path: &str,
    terminal_state: SharedBuildState,
    failure_classification: Option<&str>,
    result_metadata: &serde_json::Value,
    retention: Duration,
) -> Result<SharedBuild, SharedBuildError> {
    validate_shared_build_identity(database_url, derivation_path)?;
    let result_metadata_text = serde_json::to_string(result_metadata)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Configuration))?;
    if !result_metadata.is_object()
        || result_metadata_text.len() > 1_048_576
        || retention.is_zero()
        || retention > Duration::from_secs(86_400)
        || failure_classification.is_some_and(|classification| {
            classification.is_empty()
                || classification.len() > MAX_IPC_COMPONENT_BYTES
                || classification.contains('\0')
        })
        || (terminal_state == SharedBuildState::Failed && failure_classification.is_none())
        || (terminal_state == SharedBuildState::Succeeded && failure_classification.is_some())
    {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    let state = match terminal_state {
        SharedBuildState::Succeeded => "succeeded",
        SharedBuildState::Failed => "failed",
        _ => return Err(SharedBuildError(SharedBuildFailure::Configuration)),
    };
    let retention_seconds = f64::from(
        u32::try_from(retention.as_secs())
            .map_err(|_| SharedBuildError(SharedBuildFailure::Configuration))?,
    );
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let mut transaction = client
        .transaction()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Connection))?;
    let current_state = transaction
        .query_opt(
            "SELECT state FROM shared_builds WHERE derivation_path = $1 FOR UPDATE",
            &[&derivation_path],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
        .ok_or(SharedBuildError(SharedBuildFailure::InvalidState))?
        .try_get::<_, String>(0)
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    if (terminal_state == SharedBuildState::Succeeded && current_state != "collecting")
        || (terminal_state == SharedBuildState::Failed
            && !matches!(current_state.as_str(), "claimed" | "running" | "collecting"))
    {
        return Err(SharedBuildError(SharedBuildFailure::InvalidState));
    }
    let row = transaction
        .query_one(
            "UPDATE shared_builds
             SET state = $2,
                 result_metadata = $3::text::jsonb,
                 failure_classification = $4::text,
                 completed_at = transaction_timestamp(),
                 expires_at = transaction_timestamp() + make_interval(secs => $5)
             WHERE derivation_path = $1
             RETURNING derivation_path, request_digest, state, backend_name, backend_kind,
                       execution_recovery, cancellation, log_recovery,
                       backend_execution_id, expected_outputs, build_request::text, result_metadata::text,
                       failure_classification, created_at, started_at, collecting_at,
                       completed_at, expires_at",
            &[
                &derivation_path,
                &state,
                &result_metadata_text,
                &failure_classification,
                &retention_seconds,
            ],
        )
        .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    if current_state != "claimed" {
        let classification = failure_classification.unwrap_or("succeeded");
        let attempt_id: i64 = transaction
            .query_opt(
                "UPDATE shared_build_attempts
                 SET state = $2, completed_at = transaction_timestamp()
                 WHERE derivation_path = $1 AND state IN ('running', 'collecting')
                 RETURNING attempt_id",
                &[&derivation_path, &state],
            )
            .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?
            .ok_or(SharedBuildError(SharedBuildFailure::InvalidState))?
            .try_get(0)
            .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
        transaction
            .execute(
                "INSERT INTO shared_build_attempt_outcomes (
                     attempt_id, classification, result_metadata
                 ) VALUES ($1, $2, $3::text::jsonb)",
                &[&attempt_id, &classification, &result_metadata_text],
            )
            .map_err(|_| SharedBuildError(SharedBuildFailure::Query))?;
    }
    let build = decode_shared_build(&row).map_err(SharedBuildError)?;
    transaction
        .commit()
        .map_err(|_| SharedBuildError(SharedBuildFailure::Commit))?;
    Ok(build)
}

fn validate_shared_build_identity(
    database_url: &str,
    derivation_path: &str,
) -> Result<(), SharedBuildError> {
    if database_url.trim().is_empty()
        || derivation_path.is_empty()
        || derivation_path.len() > nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
        || derivation_path.contains('\0')
    {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_shared_build_claim(
    database_url: &str,
    derivation_path: &str,
    request_digest: &[u8],
    backend_name: &str,
    backend_kind: BackendKind,
    capabilities: BackendCapabilities,
    backend_execution_id: Option<&str>,
    expected_outputs: &[&str],
) -> Result<(), SharedBuildError> {
    let valid_execution_id = backend_execution_id.is_none_or(|execution_id| {
        !execution_id.is_empty()
            && execution_id.len() <= nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
            && !execution_id.contains('\0')
    });
    let valid_outputs = !expected_outputs.is_empty()
        && expected_outputs.len() <= 64
        && expected_outputs.iter().all(|output| {
            !output.is_empty()
                && output.len() <= nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
                && !output.contains('\0')
        })
        && expected_outputs
            .iter()
            .enumerate()
            .all(|(index, output)| !expected_outputs[..index].contains(output));
    if database_url.trim().is_empty()
        || derivation_path.is_empty()
        || derivation_path.len() > nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
        || derivation_path.contains('\0')
        || request_digest.len() != 32
        || backend_name.is_empty()
        || backend_name.len() > MAX_IPC_COMPONENT_BYTES
        || backend_name.contains('\0')
        || capabilities != backend_kind.capabilities()
        || !valid_execution_id
        || (capabilities.execution_recovery() == ExecutionRecovery::Adoptable
            && backend_execution_id.is_none())
        || !valid_outputs
    {
        return Err(SharedBuildError(SharedBuildFailure::Configuration));
    }
    Ok(())
}

fn decode_shared_build(row: &Row) -> Result<SharedBuild, SharedBuildFailure> {
    let derivation_path: String = row.try_get(0).map_err(|_| SharedBuildFailure::Query)?;
    let request_digest: Vec<u8> = row.try_get(1).map_err(|_| SharedBuildFailure::Query)?;
    let state: String = row.try_get(2).map_err(|_| SharedBuildFailure::Query)?;
    let backend_name: String = row.try_get(3).map_err(|_| SharedBuildFailure::Query)?;
    let backend_kind: String = row.try_get(4).map_err(|_| SharedBuildFailure::Query)?;
    let execution_recovery: String = row.try_get(5).map_err(|_| SharedBuildFailure::Query)?;
    let cancellation: String = row.try_get(6).map_err(|_| SharedBuildFailure::Query)?;
    let log_recovery: String = row.try_get(7).map_err(|_| SharedBuildFailure::Query)?;
    let backend_execution_id: Option<String> =
        row.try_get(8).map_err(|_| SharedBuildFailure::Query)?;
    let expected_outputs: Vec<String> = row.try_get(9).map_err(|_| SharedBuildFailure::Query)?;
    let build_request: Option<String> = row.try_get(10).map_err(|_| SharedBuildFailure::Query)?;
    let result_metadata: Option<String> = row.try_get(11).map_err(|_| SharedBuildFailure::Query)?;
    let failure_classification: Option<String> =
        row.try_get(12).map_err(|_| SharedBuildFailure::Query)?;
    let created_at: SystemTime = row.try_get(13).map_err(|_| SharedBuildFailure::Query)?;
    let started_at: Option<SystemTime> = row.try_get(14).map_err(|_| SharedBuildFailure::Query)?;
    let collecting_at: Option<SystemTime> =
        row.try_get(15).map_err(|_| SharedBuildFailure::Query)?;
    let completed_at: Option<SystemTime> =
        row.try_get(16).map_err(|_| SharedBuildFailure::Query)?;
    let expires_at: Option<SystemTime> = row.try_get(17).map_err(|_| SharedBuildFailure::Query)?;
    let request_digest: [u8; 32] = request_digest
        .try_into()
        .map_err(|_| SharedBuildFailure::Query)?;
    let backend_kind = parse_backend_kind(&backend_kind).ok_or(SharedBuildFailure::Query)?;
    let capabilities = BackendCapabilities::new(
        parse_execution_recovery(&execution_recovery).ok_or(SharedBuildFailure::Query)?,
        parse_cancellation(&cancellation).ok_or(SharedBuildFailure::Query)?,
        parse_log_recovery(&log_recovery).ok_or(SharedBuildFailure::Query)?,
    );
    validate_shared_build_claim(
        "validated",
        &derivation_path,
        &request_digest,
        &backend_name,
        backend_kind,
        capabilities,
        backend_execution_id.as_deref(),
        &expected_outputs
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )
    .map_err(|_| SharedBuildFailure::Query)?;
    let state = match state.as_str() {
        "claimed" => SharedBuildState::Claimed,
        "running" => SharedBuildState::Running,
        "collecting" => SharedBuildState::Collecting,
        "succeeded" => SharedBuildState::Succeeded,
        "failed" => SharedBuildState::Failed,
        _ => return Err(SharedBuildFailure::Query),
    };
    let build_request: Option<crate::build::BuildRequest> = build_request
        .map(|request| serde_json::from_str(&request).map_err(|_| SharedBuildFailure::Query))
        .transpose()?;
    if build_request.as_ref().is_some_and(|request| {
        request.validate_for_execution().is_err()
            || request.derivation_path() != derivation_path.as_bytes()
            || request.shared_build_digest() != request_digest
            || request
                .expected_outputs()
                .iter()
                .map(|(_, path)| path.as_slice())
                .ne(expected_outputs.iter().map(|path| path.as_bytes()))
    }) {
        return Err(SharedBuildFailure::Query);
    }
    let result_metadata: Option<serde_json::Value> = result_metadata
        .map(|metadata| serde_json::from_str(&metadata).map_err(|_| SharedBuildFailure::Query))
        .transpose()?;
    if result_metadata
        .as_ref()
        .is_some_and(|metadata| !metadata.is_object())
        || failure_classification
            .as_ref()
            .is_some_and(|classification| {
                classification.is_empty() || classification.len() > MAX_IPC_COMPONENT_BYTES
            })
        || (state == SharedBuildState::Succeeded
            && (result_metadata.is_none() || failure_classification.is_some()))
        || (state == SharedBuildState::Failed
            && (result_metadata.is_none() || failure_classification.is_none()))
        || (!matches!(
            state,
            SharedBuildState::Succeeded | SharedBuildState::Failed
        ) && (result_metadata.is_some()
            || failure_classification.is_some()
            || completed_at.is_some()
            || expires_at.is_some()))
    {
        return Err(SharedBuildFailure::Query);
    }
    Ok(SharedBuild {
        derivation_path,
        request_digest,
        state,
        backend_name,
        backend_kind,
        capabilities,
        backend_execution_id,
        expected_outputs,
        build_request,
        result_metadata,
        failure_classification,
        created_at,
        started_at,
        collecting_at,
        completed_at,
        expires_at,
    })
}

fn backend_kind_name(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Local => "local",
        BackendKind::StaticSsh => "static-ssh",
        BackendKind::Nomad => "nomad",
    }
}

fn parse_backend_kind(value: &str) -> Option<BackendKind> {
    match value {
        "local" => Some(BackendKind::Local),
        "static-ssh" => Some(BackendKind::StaticSsh),
        "nomad" => Some(BackendKind::Nomad),
        _ => None,
    }
}

fn execution_recovery_name(capability: ExecutionRecovery) -> &'static str {
    match capability {
        ExecutionRecovery::OutputOnly => "output-only",
        ExecutionRecovery::Adoptable => "adoptable",
    }
}

fn parse_execution_recovery(value: &str) -> Option<ExecutionRecovery> {
    match value {
        "output-only" => Some(ExecutionRecovery::OutputOnly),
        "adoptable" => Some(ExecutionRecovery::Adoptable),
        _ => None,
    }
}

fn cancellation_name(capability: CancellationCapability) -> &'static str {
    match capability {
        CancellationCapability::ConnectionBound => "connection-bound",
        CancellationCapability::Explicit => "explicit",
    }
}

fn parse_cancellation(value: &str) -> Option<CancellationCapability> {
    match value {
        "connection-bound" => Some(CancellationCapability::ConnectionBound),
        "explicit" => Some(CancellationCapability::Explicit),
        _ => None,
    }
}

fn log_recovery_name(capability: LogRecovery) -> &'static str {
    match capability {
        LogRecovery::LiveOnly => "live-only",
        LogRecovery::Replayable => "replayable",
    }
}

fn parse_log_recovery(value: &str) -> Option<LogRecovery> {
    match value {
        "live-only" => Some(LogRecovery::LiveOnly),
        "replayable" => Some(LogRecovery::Replayable),
        _ => None,
    }
}
