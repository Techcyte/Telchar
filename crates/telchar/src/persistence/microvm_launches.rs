use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MicrovmLaunchError;

impl fmt::Display for MicrovmLaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MicroVM launch persistence failed")
    }
}

impl std::error::Error for MicrovmLaunchError {}

#[derive(Clone)]
pub struct MicrovmLaunchIntent {
    pub intent_id: String,
    pub attempt_id: i64,
    pub backend_name: String,
    pub account_id: String,
    pub region: String,
    pub image_arn: String,
    pub image_version: String,
    pub configuration_digest: [u8; 32],
    pub request_digest: [u8; 32],
    pub client_token: String,
    pub credential_verifier: [u8; 32],
    pub admission_cutoff: SystemTime,
    pub maximum_duration_seconds: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MicrovmCleanup {
    pub intent_id: String,
    pub provider_id: String,
    pub account_id: String,
    pub region: String,
}

#[derive(serde::Serialize)]
pub struct MicrovmResolution {
    pub resolution_id: i64,
    pub intent_id: String,
    pub action: String,
    pub provider_id: Option<String>,
    pub actor_uid: i64,
    pub owner_generation: i64,
    pub recorded_at_unix_seconds: f64,
}

pub fn read_microvm_resolutions(
    database: &(impl DatabaseSource + ?Sized),
    limit: usize,
) -> Result<Vec<MicrovmResolution>, MicrovmLaunchError> {
    if !(1..=256).contains(&limit) {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let rows = client
        .query(
            "SELECT resolution_id, intent_id, action, provider_id, actor_uid, owner_generation,
                    EXTRACT(EPOCH FROM recorded_at)::double precision
             FROM microvm_resolutions ORDER BY resolution_id DESC LIMIT $1",
            &[&(limit as i64)],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(rows
        .into_iter()
        .map(|row| MicrovmResolution {
            resolution_id: row.get(0),
            intent_id: row.get(1),
            action: row.get(2),
            provider_id: row.get(3),
            actor_uid: row.get(4),
            owner_generation: row.get(5),
            recorded_at_unix_seconds: row.get(6),
        })
        .collect())
}

#[derive(serde::Serialize)]
pub struct MicrovmObligation {
    pub intent_id: String,
    pub backend_name: String,
    pub account_id: String,
    pub region: String,
    pub provider_id: Option<String>,
    pub phase: String,
    pub capacity_reserved: bool,
    pub cleanup_pending: bool,
}

pub fn read_microvm_obligations(
    database: &(impl DatabaseSource + ?Sized),
    limit: usize,
) -> Result<Vec<MicrovmObligation>, MicrovmLaunchError> {
    if !(1..=256).contains(&limit) {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let rows = client
        .query(
            "SELECT intent_id, backend_name, account_id, region, provider_id, phase,
                    reservation_released_at IS NULL,
                    provider_id IS NOT NULL AND termination_observed_at IS NULL
             FROM microvm_launches
             WHERE reservation_released_at IS NULL
                OR (provider_id IS NOT NULL AND termination_observed_at IS NULL)
             ORDER BY created_at, intent_id LIMIT $1",
            &[&(limit as i64)],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(rows
        .into_iter()
        .map(|row| MicrovmObligation {
            intent_id: row.get(0),
            backend_name: row.get(1),
            account_id: row.get(2),
            region: row.get(3),
            provider_id: row.get(4),
            phase: row.get(5),
            capacity_reserved: row.get(6),
            cleanup_pending: row.get(7),
        })
        .collect())
}

/// Provider correspondence must be verified before committing this cleanup-only resolution.
pub struct MicrovmIdentityAttachment<'a> {
    pub intent_id: &'a str,
    pub provider_id: &'a str,
    pub account_id: &'a str,
    pub region: &'a str,
    pub image_arn: &'a str,
    pub image_version: &'a str,
    pub actor_uid: u32,
    pub reason: &'a str,
    pub evidence_reference: &'a str,
}

pub fn attach_microvm_cleanup_identity(
    database: &(impl DatabaseSource + ?Sized),
    attachment: &MicrovmIdentityAttachment<'_>,
) -> Result<bool, MicrovmLaunchError> {
    if attachment.actor_uid != 0
        || !valid_resolution_text(attachment.intent_id, 128)
        || !valid_resolution_text(attachment.provider_id, 256)
        || !valid_resolution_text(attachment.account_id, 12)
        || !valid_resolution_text(attachment.region, 64)
        || !valid_resolution_text(attachment.image_arn, 2048)
        || !valid_resolution_text(attachment.image_version, 128)
        || !valid_resolution_text(attachment.reason, 1024)
        || !valid_resolution_text(attachment.evidence_reference, 1024)
    {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let mut transaction = client.transaction().map_err(|_| MicrovmLaunchError)?;
    let generation = resolution_owner(&mut transaction)?;
    let changed = transaction
        .execute(
            "UPDATE microvm_launches SET provider_id = $2, phase = 'closed'
         WHERE intent_id = $1 AND provider_id IS NULL AND phase = 'ambiguous'
           AND termination_observed_at IS NULL
           AND account_id = $3 AND region = $4 AND image_arn = $5 AND image_version = $6",
            &[
                &attachment.intent_id,
                &attachment.provider_id,
                &attachment.account_id,
                &attachment.region,
                &attachment.image_arn,
                &attachment.image_version,
            ],
        )
        .map_err(|_| MicrovmLaunchError)?;
    if changed == 0 {
        return Ok(false);
    }
    transaction
        .execute(
            "INSERT INTO microvm_resolutions
         (intent_id, action, provider_id, actor_uid, owner_generation, reason, evidence_reference)
         VALUES ($1, 'attach-identity', $2, $3, $4, $5, $6)",
            &[
                &attachment.intent_id,
                &attachment.provider_id,
                &i64::from(attachment.actor_uid),
                &generation,
                &attachment.reason,
                &attachment.evidence_reference,
            ],
        )
        .map_err(|_| MicrovmLaunchError)?;
    transaction.commit().map_err(|_| MicrovmLaunchError)?;
    Ok(true)
}

pub struct MicrovmDeploymentIdentity {
    pub account_id: String,
    pub region: String,
    pub image_arn: String,
    pub image_version: String,
}

pub fn unresolved_microvm_identity(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
) -> Result<Option<MicrovmDeploymentIdentity>, MicrovmLaunchError> {
    if !valid_resolution_text(intent_id, 128) {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let row = client
        .query_opt(
            "SELECT account_id, region, image_arn, image_version FROM microvm_launches
         WHERE intent_id = $1 AND provider_id IS NULL AND phase = 'ambiguous'
           AND termination_observed_at IS NULL",
            &[&intent_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(row.map(|row| MicrovmDeploymentIdentity {
        account_id: row.get(0),
        region: row.get(1),
        image_arn: row.get(2),
        image_version: row.get(3),
    }))
}

pub(crate) fn valid_resolution_text(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn resolution_owner(transaction: &mut DatabaseTransaction<'_>) -> Result<i64, MicrovmLaunchError> {
    transaction
        .batch_execute("SET LOCAL lock_timeout = '250ms'; SET LOCAL statement_timeout = '1s'")
        .map_err(|_| MicrovmLaunchError)?;
    let owner = transaction
        .query_opt(
            "SELECT generation FROM singleton_ownership
         WHERE owner_kind = 'daemon'
           AND current_setting('telchar.owner_kind', true) = owner_kind
           AND current_setting('telchar.owner_token', true) = owner_token
           AND current_setting('telchar.owner_generation', true) = generation::text
           AND lease_expires_at > clock_timestamp() FOR SHARE",
            &[],
        )
        .map_err(|_| MicrovmLaunchError)?
        .ok_or(MicrovmLaunchError)?;
    Ok(owner.get(0))
}

/// Requires an exact TERMINATED observation under attested account and region authority.
pub struct MicrovmTerminationObservation<'a> {
    pub intent_id: &'a str,
    pub provider_id: &'a str,
    pub account_id: &'a str,
    pub region: &'a str,
    pub actor_uid: u32,
    pub reason: &'a str,
    pub evidence_reference: &'a str,
}

pub fn resolve_microvm_termination(
    database: &(impl DatabaseSource + ?Sized),
    observation: &MicrovmTerminationObservation<'_>,
) -> Result<bool, MicrovmLaunchError> {
    if observation.actor_uid != 0
        || !valid_resolution_text(observation.intent_id, 128)
        || !valid_resolution_text(observation.provider_id, 256)
        || !valid_resolution_text(observation.account_id, 12)
        || !valid_resolution_text(observation.region, 64)
        || !valid_resolution_text(observation.reason, 1024)
        || !valid_resolution_text(observation.evidence_reference, 1024)
    {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let mut transaction = client.transaction().map_err(|_| MicrovmLaunchError)?;
    let generation = resolution_owner(&mut transaction)?;
    let changed = transaction
        .execute(
            "UPDATE microvm_launches SET phase = 'closed',
         reservation_released_at = COALESCE(reservation_released_at, clock_timestamp()),
         termination_observed_at = clock_timestamp()
         WHERE intent_id = $1 AND provider_id = $2 AND account_id = $3 AND region = $4
           AND termination_observed_at IS NULL",
            &[
                &observation.intent_id,
                &observation.provider_id,
                &observation.account_id,
                &observation.region,
            ],
        )
        .map_err(|_| MicrovmLaunchError)?;
    if changed == 0 {
        return Ok(false);
    }
    transaction
        .execute(
            "INSERT INTO microvm_resolutions
         (intent_id, action, provider_id, actor_uid, owner_generation, reason, evidence_reference)
         VALUES ($1, 'verify-termination', $2, $3, $4, $5, $6)",
            &[
                &observation.intent_id,
                &observation.provider_id,
                &i64::from(observation.actor_uid),
                &generation,
                &observation.reason,
                &observation.evidence_reference,
            ],
        )
        .map_err(|_| MicrovmLaunchError)?;
    transaction.commit().map_err(|_| MicrovmLaunchError)?;
    Ok(true)
}

pub struct MicrovmRiskRelease<'a> {
    pub intent_id: &'a str,
    pub provider_id: Option<&'a str>,
    pub actor_uid: u32,
    pub reason: &'a str,
    pub evidence_reference: &'a str,
    pub accept_remote_execution_risk: bool,
}

pub fn release_microvm_capacity_risk(
    database: &(impl DatabaseSource + ?Sized),
    release: &MicrovmRiskRelease<'_>,
) -> Result<bool, MicrovmLaunchError> {
    if release.actor_uid != 0
        || !release.accept_remote_execution_risk
        || !valid_resolution_text(release.intent_id, 128)
        || !valid_resolution_text(release.reason, 1024)
        || !valid_resolution_text(release.evidence_reference, 1024)
        || release
            .provider_id
            .is_some_and(|id| !valid_resolution_text(id, 256))
    {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let mut transaction = client.transaction().map_err(|_| MicrovmLaunchError)?;
    let generation = resolution_owner(&mut transaction)?;
    let changed = transaction
        .execute(
            "UPDATE microvm_launches SET reservation_released_at = clock_timestamp()
         WHERE intent_id = $1 AND provider_id IS NOT DISTINCT FROM $2::text
           AND phase IN ('ambiguous', 'closed')
           AND reservation_released_at IS NULL AND termination_observed_at IS NULL",
            &[&release.intent_id, &release.provider_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    if changed == 0 {
        return Ok(false);
    }
    transaction
        .execute(
            "INSERT INTO microvm_resolutions
         (intent_id, action, provider_id, actor_uid, owner_generation, reason, evidence_reference)
         VALUES ($1, 'risk-release', $2, $3, $4, $5, $6)",
            &[
                &release.intent_id,
                &release.provider_id,
                &i64::from(release.actor_uid),
                &generation,
                &release.reason,
                &release.evidence_reference,
            ],
        )
        .map_err(|_| MicrovmLaunchError)?;
    transaction.commit().map_err(|_| MicrovmLaunchError)?;
    Ok(true)
}

pub fn microvm_cleanup_is_configured(
    database: &(impl DatabaseSource + ?Sized),
    identities: &[(&str, &str)],
) -> Result<bool, MicrovmLaunchError> {
    let accounts: Vec<_> = identities.iter().map(|(account, _)| *account).collect();
    let regions: Vec<_> = identities.iter().map(|(_, region)| *region).collect();
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let row = client
        .query_one(
            "SELECT NOT EXISTS (
            SELECT 1 FROM microvm_launches l
            WHERE (l.reservation_released_at IS NULL
                   OR (l.provider_id IS NOT NULL AND l.termination_observed_at IS NULL))
              AND NOT EXISTS (
                SELECT 1 FROM unnest($1::text[], $2::text[]) AS configured(account_id, region)
                WHERE configured.account_id = l.account_id AND configured.region = l.region
              )
        )",
            &[&accounts, &regions],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(row.get(0))
}

pub fn pending_microvm_cleanup(
    database: &(impl DatabaseSource + ?Sized),
    account_id: &str,
    region: &str,
    limit: usize,
    after_intent: Option<&str>,
) -> Result<Vec<MicrovmCleanup>, MicrovmLaunchError> {
    let _database_operation =
        telemetry::DatabaseOperation::start(stringify!(pending_microvm_cleanup));
    if !(1..=1000).contains(&limit) {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let observations = client
        .query(
            "SELECT provider_id IS NOT NULL, count(*),
         GREATEST(0, EXTRACT(EPOCH FROM clock_timestamp() - min(created_at)))::double precision
         FROM microvm_launches
         WHERE account_id = $1 AND region = $2
           AND (reservation_released_at IS NULL
                OR (provider_id IS NOT NULL AND termination_observed_at IS NULL))
         GROUP BY provider_id IS NOT NULL",
            &[&account_id, &region],
        )
        .map_err(|_| MicrovmLaunchError)?;
    for identified in [false, true] {
        let observation = observations
            .iter()
            .find(|row| row.get::<_, bool>(0) == identified);
        let (count, age) = observation.map_or((0, 0.0), |row| {
            (row.get::<_, i64>(1) as u64, row.get::<_, f64>(2))
        });
        crate::service::metrics::microvm_obligations(identified, count, age);
    }
    let rows = client
        .query(
            "SELECT l.intent_id, l.provider_id, l.account_id, l.region
         FROM microvm_launches l JOIN shared_build_attempts a ON a.attempt_id = l.attempt_id
         JOIN shared_builds b ON b.derivation_path = a.derivation_path
         WHERE l.account_id = $1 AND l.region = $4 AND l.provider_id IS NOT NULL
           AND l.termination_observed_at IS NULL
           AND (l.phase = 'closed' OR a.state NOT IN ('running', 'collecting')
                OR b.state NOT IN ('running', 'collecting'))
         ORDER BY CASE WHEN $3::text IS NULL THEN false ELSE l.intent_id <= $3 END,
                  l.intent_id LIMIT $2",
            &[&account_id, &(limit as i64), &after_intent, &region],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(rows
        .into_iter()
        .map(|row| MicrovmCleanup {
            intent_id: row.get(0),
            provider_id: row.get(1),
            account_id: row.get(2),
            region: row.get(3),
        })
        .collect())
}

pub fn microvm_termination_identity(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
    provider_id: &str,
) -> Result<Option<MicrovmCleanup>, MicrovmLaunchError> {
    if !valid_resolution_text(intent_id, 128) || !valid_resolution_text(provider_id, 256) {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let row = client
        .query_opt(
            "SELECT intent_id, provider_id, account_id, region FROM microvm_launches
         WHERE intent_id = $1 AND provider_id = $2 AND termination_observed_at IS NULL",
            &[&intent_id, &provider_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(row.map(|row| MicrovmCleanup {
        intent_id: row.get(0),
        provider_id: row.get(1),
        account_id: row.get(2),
        region: row.get(3),
    }))
}

pub fn pending_microvm_cleanup_identity(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
    provider_id: &str,
) -> Result<bool, MicrovmLaunchError> {
    let _database_operation =
        telemetry::DatabaseOperation::start(stringify!(pending_microvm_cleanup_identity));
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let row = client.query_opt(
        "SELECT 1 FROM microvm_launches l JOIN shared_build_attempts a ON a.attempt_id = l.attempt_id
         JOIN shared_builds b ON b.derivation_path = a.derivation_path
         WHERE l.intent_id = $1 AND l.provider_id = $2 AND l.termination_observed_at IS NULL
           AND (l.phase = 'closed' OR a.state NOT IN ('running', 'collecting')
                OR b.state NOT IN ('running', 'collecting'))",
        &[&intent_id, &provider_id],
    ).map_err(|_| MicrovmLaunchError)?;
    Ok(row.is_some())
}

/// Records an exact successful provider TERMINATED observation, not a terminate acknowledgment.
pub fn record_microvm_termination(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
    provider_id: &str,
) -> Result<bool, MicrovmLaunchError> {
    let _database_operation =
        telemetry::DatabaseOperation::start(stringify!(record_microvm_termination));
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let changed = client
        .execute(
            "UPDATE microvm_launches SET phase = 'closed',
             reservation_released_at = COALESCE(reservation_released_at, clock_timestamp()),
             termination_observed_at = clock_timestamp()
         WHERE intent_id = $1 AND provider_id = $2 AND termination_observed_at IS NULL",
            &[&intent_id, &provider_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(changed == 1)
}

pub fn reserve_microvm_launch(
    database: &(impl DatabaseSource + ?Sized),
    intent: &MicrovmLaunchIntent,
    maximum_reservations: usize,
) -> Result<bool, MicrovmLaunchError> {
    let _database_operation =
        telemetry::DatabaseOperation::start(stringify!(reserve_microvm_launch));
    let maximum = i64::try_from(maximum_reservations).map_err(|_| MicrovmLaunchError)?;
    if maximum == 0 {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let mut transaction = client.transaction().map_err(|_| MicrovmLaunchError)?;
    transaction
        .query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended('microvm_launches:reservations', 0))",
            &[],
        )
        .map_err(|_| MicrovmLaunchError)?;
    let active = transaction
        .query_opt(
            "SELECT b.derivation_path FROM shared_builds b
         JOIN shared_build_attempts a ON a.derivation_path = b.derivation_path
         WHERE a.attempt_id = $1 AND b.state = 'running' AND a.state = 'running'
           AND b.backend_name = $2 AND a.backend_name = $2
         FOR UPDATE OF b, a",
            &[&intent.attempt_id, &intent.backend_name],
        )
        .map_err(|_| MicrovmLaunchError)?;
    if active.is_none() {
        return Ok(false);
    }
    let reserved: i64 = transaction
        .query_one(
            "SELECT count(*) FROM microvm_launches
         WHERE (backend_name = $1 OR (account_id = $2 AND region = $3))
           AND reservation_released_at IS NULL",
            &[&intent.backend_name, &intent.account_id, &intent.region],
        )
        .map_err(|_| MicrovmLaunchError)?
        .get(0);
    if reserved >= maximum {
        return Ok(false);
    }
    let inserted = transaction
        .execute(
            "INSERT INTO microvm_launches
         (intent_id, attempt_id, backend_name, account_id, region, image_arn, image_version,
          configuration_digest, request_digest, client_token, credential_verifier,
          admission_cutoff, maximum_duration_seconds)
         SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13
         WHERE $12 > clock_timestamp() ON CONFLICT DO NOTHING",
            &[
                &intent.intent_id,
                &intent.attempt_id,
                &intent.backend_name,
                &intent.account_id,
                &intent.region,
                &intent.image_arn,
                &intent.image_version,
                &&intent.configuration_digest[..],
                &&intent.request_digest[..],
                &intent.client_token,
                &&intent.credential_verifier[..],
                &intent.admission_cutoff,
                &intent.maximum_duration_seconds,
            ],
        )
        .map_err(|_| MicrovmLaunchError)?;
    transaction.commit().map_err(|_| MicrovmLaunchError)?;
    Ok(inserted == 1)
}

/// Loads execution authority only for a consumed session on its active attempt.
pub fn microvm_attempt_request(
    database: &(impl DatabaseSource + ?Sized),
    attempt_id: i64,
    backend_name: &str,
) -> Result<Option<crate::build::BuildRequest>, MicrovmLaunchError> {
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let row = client
        .query_opt(
            "SELECT b.build_request::text, b.request_digest, b.derivation_path
         FROM shared_build_attempts a JOIN shared_builds b ON b.derivation_path = a.derivation_path
         WHERE a.attempt_id = $1 AND a.backend_name = $2 AND b.backend_name = $2
           AND a.state = 'running' AND b.state = 'running'",
            &[&attempt_id, &backend_name],
        )
        .map_err(|_| MicrovmLaunchError)?;
    row.map(|row| decode_microvm_request(&row)).transpose()
}

pub fn microvm_session_request(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
) -> Result<Option<crate::build::BuildRequest>, MicrovmLaunchError> {
    let _database_operation =
        telemetry::DatabaseOperation::start(stringify!(microvm_session_request));
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let row = client
        .query_opt(
            "SELECT b.build_request::text, b.request_digest, b.derivation_path
         FROM microvm_launches l
         JOIN shared_build_attempts a ON a.attempt_id = l.attempt_id
         JOIN shared_builds b ON b.derivation_path = a.derivation_path
         WHERE l.intent_id = $1 AND l.phase = 'bound'
           AND l.session_consumed_at IS NOT NULL AND l.reservation_released_at IS NULL
           AND b.state = 'running' AND a.state = 'running'
           AND b.backend_name = l.backend_name AND a.backend_name = l.backend_name",
            &[&intent_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    let Some(row) = row else {
        return Ok(None);
    };
    decode_microvm_request(&row).map(Some)
}

fn decode_microvm_request(
    row: &postgres::Row,
) -> Result<crate::build::BuildRequest, MicrovmLaunchError> {
    let encoded: Option<String> = row.try_get(0).map_err(|_| MicrovmLaunchError)?;
    let request: crate::build::BuildRequest =
        serde_json::from_str(&encoded.ok_or(MicrovmLaunchError)?)
            .map_err(|_| MicrovmLaunchError)?;
    let digest: Vec<u8> = row.try_get(1).map_err(|_| MicrovmLaunchError)?;
    let derivation: String = row.try_get(2).map_err(|_| MicrovmLaunchError)?;
    request
        .validate_for_execution()
        .map_err(|_| MicrovmLaunchError)?;
    if request.shared_build_digest().as_slice() != digest
        || request.derivation_path() != derivation.as_bytes()
    {
        return Err(MicrovmLaunchError);
    }
    Ok(request)
}

pub fn microvm_execution_active(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
) -> Result<bool, MicrovmLaunchError> {
    let _database_operation =
        telemetry::DatabaseOperation::start(stringify!(microvm_execution_active));
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let row = client
        .query_opt(
            "SELECT 1 FROM microvm_launches l
         JOIN shared_build_attempts a ON a.attempt_id = l.attempt_id
         JOIN shared_builds b ON b.derivation_path = a.derivation_path
         WHERE l.intent_id = $1 AND l.phase IN ('dispatching', 'bound')
           AND l.reservation_released_at IS NULL
           AND (l.session_consumed_at IS NOT NULL OR l.admission_cutoff > clock_timestamp())
           AND b.state = 'running' AND a.state = 'running'
           AND b.backend_name = l.backend_name AND a.backend_name = l.backend_name",
            &[&intent_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(row.is_some())
}

pub fn microvm_session_backend(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
    credential_verifier: &[u8; 32],
) -> Result<Option<String>, MicrovmLaunchError> {
    let _database_operation =
        telemetry::DatabaseOperation::start(stringify!(microvm_session_backend));
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let row = client
        .query_opt(
            "SELECT backend_name FROM microvm_launches WHERE intent_id = $1
         AND credential_verifier = $2 AND phase IN ('dispatching', 'bound')
         AND admission_cutoff > clock_timestamp() AND session_consumed_at IS NULL
         AND reservation_released_at IS NULL",
            &[&intent_id, &&credential_verifier[..]],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(row.map(|row| row.get(0)))
}

pub fn microvm_session_awaits_binding(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
    credential_verifier: &[u8; 32],
) -> Result<bool, MicrovmLaunchError> {
    let _database_operation =
        telemetry::DatabaseOperation::start(stringify!(microvm_session_awaits_binding));
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let pending = client
        .query_opt(
            "SELECT 1 FROM microvm_launches l
         JOIN shared_build_attempts a ON a.attempt_id = l.attempt_id
         JOIN shared_builds b ON b.derivation_path = a.derivation_path
         WHERE l.intent_id = $1 AND l.credential_verifier = $2
           AND l.phase = 'dispatching' AND l.provider_id IS NULL
           AND l.session_consumed_at IS NULL AND l.reservation_released_at IS NULL
           AND l.admission_cutoff > clock_timestamp()
           AND b.state = 'running' AND a.state = 'running'
           AND b.backend_name = l.backend_name AND a.backend_name = l.backend_name",
            &[&intent_id, &&credential_verifier[..]],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(pending.is_some())
}

/// Consumes a verified capability for one exact bound worker session.
pub fn admit_microvm_session(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
    provider_id: &str,
    credential_verifier: &[u8; 32],
) -> Result<bool, MicrovmLaunchError> {
    let _database_operation =
        telemetry::DatabaseOperation::start(stringify!(admit_microvm_session));
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let mut transaction = client.transaction().map_err(|_| MicrovmLaunchError)?;
    let active = transaction
        .query_opt(
            "SELECT b.derivation_path FROM shared_builds b
         JOIN shared_build_attempts a ON a.derivation_path = b.derivation_path
         JOIN microvm_launches l ON l.attempt_id = a.attempt_id
         WHERE l.intent_id = $1 AND b.state = 'running' AND a.state = 'running'
           AND b.backend_name = l.backend_name AND a.backend_name = l.backend_name
         FOR UPDATE OF b, a",
            &[&intent_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    if active.is_none() {
        return Ok(false);
    }
    let consumed = transaction
        .execute(
            "UPDATE microvm_launches SET session_consumed_at = clock_timestamp()
         WHERE intent_id = $1 AND provider_id = $2 AND credential_verifier = $3
           AND phase = 'bound' AND admission_cutoff > clock_timestamp()
           AND session_consumed_at IS NULL AND reservation_released_at IS NULL",
            &[&intent_id, &provider_id, &&credential_verifier[..]],
        )
        .map_err(|_| MicrovmLaunchError)?;
    transaction.commit().map_err(|_| MicrovmLaunchError)?;
    Ok(consumed == 1)
}

pub fn bind_microvm_launch(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
    provider_id: &str,
) -> Result<bool, MicrovmLaunchError> {
    let _database_operation = telemetry::DatabaseOperation::start(stringify!(bind_microvm_launch));
    if intent_id.is_empty()
        || intent_id.len() > 128
        || provider_id.is_empty()
        || provider_id.len() > 256
        || provider_id.chars().any(char::is_control)
    {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let mut transaction = client.transaction().map_err(|_| MicrovmLaunchError)?;
    let active = transaction
        .query_opt(
            "SELECT b.state = 'running' AND a.state = 'running'
                AND b.backend_name = l.backend_name AND a.backend_name = l.backend_name
         FROM shared_builds b
         JOIN shared_build_attempts a ON a.derivation_path = b.derivation_path
         JOIN microvm_launches l ON l.attempt_id = a.attempt_id
         WHERE l.intent_id = $1 FOR UPDATE OF b, a",
            &[&intent_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    let Some(active) = active else {
        return Ok(false);
    };
    let active: bool = active.get(0);
    let bound = transaction
        .execute(
            "UPDATE microvm_launches SET provider_id = $2,
             phase = CASE WHEN phase = 'dispatching' AND $3
                               AND admission_cutoff > clock_timestamp()
                          THEN 'bound' ELSE 'closed' END
         WHERE intent_id = $1 AND provider_id IS NULL
           AND phase IN ('dispatching', 'ambiguous')
           AND reservation_released_at IS NULL",
            &[&intent_id, &provider_id, &active],
        )
        .map_err(|_| MicrovmLaunchError)?;
    transaction.commit().map_err(|_| MicrovmLaunchError)?;
    Ok(bound == 1)
}

pub fn cancel_microvm_attempt(
    database: &(impl DatabaseSource + ?Sized),
    attempt_id: i64,
) -> Result<bool, MicrovmLaunchError> {
    let row = connect(database)
        .map_err(|_| MicrovmLaunchError)?
        .query_opt(
            "SELECT intent_id FROM microvm_launches WHERE attempt_id = $1",
            &[&attempt_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    match row {
        Some(row) => cancel_microvm_launch(database, &row.get::<_, String>(0)),
        None => Ok(false),
    }
}

pub fn cancel_microvm_launch(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
) -> Result<bool, MicrovmLaunchError> {
    let _database_operation =
        telemetry::DatabaseOperation::start(stringify!(cancel_microvm_launch));
    if intent_id.is_empty() || intent_id.len() > 128 {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let changed = client
        .execute(
            "UPDATE microvm_launches
         SET phase = CASE WHEN phase = 'prepared' THEN 'closed'
                          WHEN provider_id IS NULL THEN 'ambiguous' ELSE 'closed' END,
             reservation_released_at = CASE WHEN phase = 'prepared'
                 THEN clock_timestamp() ELSE reservation_released_at END
         WHERE intent_id = $1 AND phase IN ('prepared', 'dispatching', 'bound')",
            &[&intent_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(changed == 1)
}

pub fn dispatch_microvm_launch_intent(
    database: &(impl DatabaseSource + ?Sized),
    intent: &MicrovmLaunchIntent,
) -> Result<bool, MicrovmLaunchError> {
    dispatch_launch(database, &intent.intent_id, Some(intent))
}

pub fn dispatch_microvm_launch(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
) -> Result<bool, MicrovmLaunchError> {
    dispatch_launch(database, intent_id, None)
}

fn dispatch_launch(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
    intent: Option<&MicrovmLaunchIntent>,
) -> Result<bool, MicrovmLaunchError> {
    let _database_operation =
        telemetry::DatabaseOperation::start(stringify!(dispatch_microvm_launch));
    if intent_id.is_empty() || intent_id.len() > 128 {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let mut transaction = client.transaction().map_err(|_| MicrovmLaunchError)?;
    let active = transaction
        .query_opt(
            "SELECT b.derivation_path FROM shared_builds b
         JOIN shared_build_attempts a ON a.derivation_path = b.derivation_path
         JOIN microvm_launches l ON l.attempt_id = a.attempt_id
         WHERE l.intent_id = $1 AND b.state = 'running' AND a.state = 'running'
           AND b.backend_name = l.backend_name AND a.backend_name = l.backend_name
         FOR UPDATE OF b, a",
            &[&intent_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    if active.is_none() {
        return Ok(false);
    }
    if let Some(intent) = intent {
        let matches = transaction.query_one(
            "SELECT attempt_id = $2 AND backend_name = $3 AND account_id = $4 AND region = $5
                AND image_arn = $6 AND image_version = $7 AND configuration_digest = $8
                AND request_digest = $9 AND client_token = $10 AND credential_verifier = $11
                AND maximum_duration_seconds = $12 FROM microvm_launches WHERE intent_id = $1 FOR UPDATE",
            &[&intent_id, &intent.attempt_id, &intent.backend_name, &intent.account_id,
              &intent.region, &intent.image_arn, &intent.image_version,
              &&intent.configuration_digest[..], &&intent.request_digest[..], &intent.client_token,
              &&intent.credential_verifier[..], &intent.maximum_duration_seconds],
        ).map_err(|_| MicrovmLaunchError)?.get::<_, bool>(0);
        if !matches {
            return Err(MicrovmLaunchError);
        }
    }
    let granted = transaction
        .execute(
            "UPDATE microvm_launches SET phase = 'dispatching'
         WHERE intent_id = $1 AND phase = 'prepared'
           AND reservation_released_at IS NULL
           AND admission_cutoff > clock_timestamp()",
            &[&intent_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    transaction.commit().map_err(|_| MicrovmLaunchError)?;
    Ok(granted == 1)
}
