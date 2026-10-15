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
            "SELECT pg_advisory_xact_lock(hashtextextended('microvm_launches:' || $1, 0))",
            &[&intent.backend_name],
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
    let reserved: i64 = transaction.query_one(
        "SELECT count(*) FROM microvm_launches WHERE backend_name = $1 AND reservation_released_at IS NULL",
        &[&intent.backend_name],
    ).map_err(|_| MicrovmLaunchError)?.get(0);
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
