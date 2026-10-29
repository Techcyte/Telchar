use super::*;

#[derive(Clone, Debug)]
pub struct MicrovmQuotaReservation {
    pub(crate) unit: &'static str,
    pub(crate) allowance: i64,
    pub(crate) amount_per_second: i64,
    pub(crate) pricing_reference: Option<String>,
    pub(crate) pricing_expires_at: Option<SystemTime>,
}

impl MicrovmQuotaReservation {
    pub fn vm_seconds(allowance: i64) -> Result<Self, MicrovmLaunchError> {
        if allowance <= 0 {
            return Err(MicrovmLaunchError);
        }
        Ok(Self {
            unit: "vm-seconds",
            allowance,
            amount_per_second: 1,
            pricing_reference: None,
            pricing_expires_at: None,
        })
    }
}

#[derive(serde::Serialize)]
pub struct MicrovmQuotaDebit {
    pub intent_id: String,
    pub account_id: String,
    pub region: String,
    pub unit: String,
    pub allowance: i64,
    pub amount: i64,
    pub pricing_reference: Option<String>,
    pub pricing_expires_at_unix_seconds: Option<f64>,
    pub dispatched: bool,
}

pub fn read_microvm_quota_debits(
    database: &(impl DatabaseSource + ?Sized),
    limit: usize,
) -> Result<Vec<MicrovmQuotaDebit>, MicrovmLaunchError> {
    if !(1..=256).contains(&limit) {
        return Err(MicrovmLaunchError);
    }
    let mut client = connect(database).map_err(|_| MicrovmLaunchError)?;
    let rows = client.query(
        "SELECT l.intent_id, l.account_id, l.region, q.unit, q.allowance, q.amount,
                q.pricing_reference, extract(epoch FROM q.pricing_expires_at)::double precision,
                q.dispatched_at IS NOT NULL
         FROM microvm_quota_reservations q JOIN microvm_launches l USING (intent_id)
         WHERE NOT (q.dispatched_at IS NULL AND l.reservation_released_at IS NOT NULL)
           AND (l.created_at >= date_trunc('month', clock_timestamp() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                OR q.dispatched_at IS NULL
                OR q.dispatched_at + make_interval(secs => l.maximum_duration_seconds) >=
                    date_trunc('month', clock_timestamp() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                OR l.termination_observed_at IS NULL)
         ORDER BY l.created_at, l.intent_id LIMIT $1", &[&(limit as i64)],
    ).map_err(|_| MicrovmLaunchError)?;
    Ok(rows
        .iter()
        .map(|row| MicrovmQuotaDebit {
            intent_id: row.get(0),
            account_id: row.get(1),
            region: row.get(2),
            unit: row.get(3),
            allowance: row.get(4),
            amount: row.get(5),
            pricing_reference: row.get(6),
            pricing_expires_at_unix_seconds: row.get(7),
            dispatched: row.get(8),
        })
        .collect())
}

pub(super) fn reserve_microvm_quota(
    transaction: &mut DatabaseTransaction<'_>,
    intent: &MicrovmLaunchIntent,
    quota: &MicrovmQuotaReservation,
) -> Result<bool, MicrovmLaunchError> {
    let unaccounted: bool = transaction
        .query_one(
            "SELECT EXISTS (
            SELECT 1 FROM microvm_launches l
            LEFT JOIN microvm_quota_reservations q USING (intent_id)
            WHERE l.account_id = $1 AND l.region = $2 AND l.intent_id <> $3
              AND q.intent_id IS NULL
              AND (l.reservation_released_at IS NULL
                   OR l.created_at >=
                       date_trunc('month', clock_timestamp() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                   OR l.termination_observed_at >=
                       date_trunc('month', clock_timestamp() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                   OR (l.termination_observed_at IS NULL
                       AND (l.provider_id IS NOT NULL OR l.phase = 'ambiguous')))
         )",
            &[&intent.account_id, &intent.region, &intent.intent_id],
        )
        .map_err(|_| MicrovmLaunchError)?
        .get(0);
    if unaccounted {
        return Ok(false);
    }
    let amount = quota
        .amount_per_second
        .checked_mul(i64::from(intent.maximum_duration_seconds))
        .filter(|amount| *amount > 0)
        .ok_or(MicrovmLaunchError)?;
    if amount > quota.allowance {
        return Ok(false);
    }
    let row = transaction.query_one(
        "WITH period AS (
            SELECT date_trunc('month', clock_timestamp() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' AS start
         )
         SELECT COALESCE(sum(q.amount), 0)::text,
                bool_and(q.unit = $3),
                bool_and(q.allowance = $4)
         FROM microvm_quota_reservations q
         JOIN microvm_launches l USING (intent_id), period p
         WHERE l.account_id = $1 AND l.region = $2
           AND NOT (q.dispatched_at IS NULL AND l.reservation_released_at IS NOT NULL)
           AND (l.created_at >= p.start
                OR q.dispatched_at IS NULL
                OR q.dispatched_at + make_interval(secs => l.maximum_duration_seconds) >= p.start
                OR (q.dispatched_at IS NOT NULL AND l.termination_observed_at IS NULL))",
        &[&intent.account_id, &intent.region, &quota.unit, &quota.allowance],
    ).map_err(|_| MicrovmLaunchError)?;
    let used = row
        .get::<_, String>(0)
        .parse::<i64>()
        .map_err(|_| MicrovmLaunchError)?;
    if row.get::<_, Option<bool>>(1) == Some(false)
        || row.get::<_, Option<bool>>(2) == Some(false)
        || used
            .checked_add(amount)
            .is_none_or(|total| total > quota.allowance)
    {
        return Ok(false);
    }
    transaction
        .execute(
            "INSERT INTO microvm_quota_reservations
            (intent_id, unit, allowance, amount, pricing_reference, pricing_expires_at)
         VALUES ($1,$2,$3,$4,$5,$6)",
            &[
                &intent.intent_id,
                &quota.unit,
                &quota.allowance,
                &amount,
                &quota.pricing_reference,
                &quota.pricing_expires_at,
            ],
        )
        .map_err(|_| MicrovmLaunchError)?;
    Ok(true)
}
