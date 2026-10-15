use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MicrovmLaunchError;

impl fmt::Display for MicrovmLaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MicroVM launch persistence failed")
    }
}

impl std::error::Error for MicrovmLaunchError {}

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

pub fn dispatch_microvm_launch(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
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
