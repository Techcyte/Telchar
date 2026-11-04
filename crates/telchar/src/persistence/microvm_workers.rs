use super::*;

/// Record trusted runtime teardown acknowledgement for the exact assignment.
pub fn reset_microvm_worker(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
    epoch: i64,
    attempt_id: i64,
) -> Result<bool, MicrovmLaunchError> {
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let changed = client
        .execute(
            "UPDATE microvm_workers w SET state='idle', attempt_id=NULL,
            idle_since=clock_timestamp(), completed_jobs=completed_jobs+1
         FROM microvm_launches l WHERE w.intent_id=$1 AND l.intent_id=w.intent_id
            AND w.state='busy' AND w.assignment_epoch=$2 AND w.attempt_id=$3
            AND l.phase='bound' AND l.reservation_released_at IS NULL
            AND w.owner_generation=COALESCE(NULLIF(current_setting('telchar.owner_generation', true), '')::bigint, 0)
            AND w.expires_at > clock_timestamp()
            AND EXISTS (SELECT 1 FROM shared_build_attempts a WHERE a.attempt_id=$3
                        AND a.state='succeeded')",
            &[&intent_id, &epoch, &attempt_id],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(changed == 1)
}

pub fn claim_microvm_worker(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
    epoch: i64,
    attempt_id: i64,
    budget: Duration,
) -> Result<bool, MicrovmLaunchError> {
    let seconds = i32::try_from(budget.as_secs()).map_err(|_| MicrovmLaunchError)?;
    if seconds <= 0 || budget.subsec_nanos() != 0 {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let mut transaction = client.transaction().map_err(|_| MicrovmLaunchError)?;
    let active = transaction.query_opt(
        "SELECT a.backend_name FROM shared_build_attempts a JOIN shared_builds b USING(derivation_path)
         WHERE a.attempt_id=$1 AND a.state='running' AND b.state='running'
            AND a.backend_name=b.backend_name FOR UPDATE OF a,b",
        &[&attempt_id],
    ).map_err(|_| MicrovmLaunchError)?;
    let Some(active) = active else {
        return Ok(false);
    };
    let backend: String = active.get(0);
    let changed = transaction
        .execute(
            "UPDATE microvm_workers w SET state='busy', idle_since=NULL, attempt_id=$3,
             assignment_epoch=assignment_epoch+1
         FROM microvm_launches l WHERE w.intent_id=$1 AND l.intent_id=w.intent_id
             AND w.state='idle' AND w.assignment_epoch=$2 AND l.backend_name=$4
             AND l.phase='bound' AND l.reservation_released_at IS NULL
             AND w.owner_generation=COALESCE(NULLIF(current_setting('telchar.owner_generation', true), '')::bigint, 0)
             AND w.expires_at > clock_timestamp()+make_interval(secs => $5::integer)",
            &[&intent_id, &epoch, &attempt_id, &backend, &seconds],
        )
        .map_err(|_| MicrovmLaunchError)?;
    transaction.commit().map_err(|_| MicrovmLaunchError)?;
    Ok(changed == 1)
}

pub(crate) fn drain_previous_microvm_workers(
    database: &(impl DatabaseSource + ?Sized),
) -> Result<(), MicrovmLaunchError> {
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    client.execute(
        "UPDATE microvm_launches l SET phase='closed' FROM microvm_workers w
         WHERE l.intent_id=w.intent_id AND l.phase <> 'closed'
           AND w.owner_generation <> NULLIF(current_setting('telchar.owner_generation', true), '')::bigint",
        &[],
    ).map_err(|_| MicrovmLaunchError)?;
    Ok(())
}

pub fn retire_microvm_worker(
    database: &(impl DatabaseSource + ?Sized),
    intent_id: &str,
    epoch: i64,
) -> Result<bool, MicrovmLaunchError> {
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let changed = client
        .execute(
            "UPDATE microvm_workers SET state='draining', idle_since=NULL
         WHERE intent_id=$1 AND assignment_epoch=$2 AND state='idle'",
            &[&intent_id, &epoch],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(changed == 1)
}
