//! Owns durable hook delivery claims independently of successful build results.

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PostBuildHookRoot {
    pub attempt_id: i64,
    pub output_index: i32,
    pub store_path: String,
}

impl PostBuildHookRoot {
    pub fn lease_id(&self) -> String {
        format!("hook_delivery-{}-{}", self.attempt_id, self.output_index)
    }
}

pub fn prepare_post_build_hook_roots(
    database: &(impl DatabaseSource + ?Sized),
    derivation_path: &str,
) -> Result<Vec<PostBuildHookRoot>, Box<dyn std::error::Error + Send + Sync>> {
    let mut client = connect(database)?;
    let mut transaction = client.transaction()?;
    let row = transaction
        .query_opt(
            "SELECT a.attempt_id, b.expected_outputs FROM shared_builds b
         JOIN shared_build_attempts a USING (derivation_path)
         WHERE b.derivation_path = $1 AND a.state IN ('running', 'collecting')
         FOR UPDATE OF b, a",
            &[&derivation_path],
        )?
        .ok_or("active hook delivery execution is missing")?;
    let attempt_id: i64 = row.try_get(0)?;
    let outputs: Vec<String> = row.try_get(1)?;
    let mut roots = Vec::with_capacity(outputs.len());
    for (index, store_path) in outputs.into_iter().enumerate() {
        let output_index = i32::try_from(index)?;
        transaction.execute(
            "INSERT INTO post_build_hook_roots (attempt_id, output_index, store_path)
             VALUES ($1, $2, $3) ON CONFLICT (attempt_id, output_index) DO NOTHING",
            &[&attempt_id, &output_index, &store_path],
        )?;
        roots.push(PostBuildHookRoot {
            attempt_id,
            output_index,
            store_path,
        });
    }
    transaction.commit()?;
    Ok(roots)
}

pub fn retain_post_build_hook_root(
    database: &(impl DatabaseSource + ?Sized),
    root: &PostBuildHookRoot,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut client = connect(database)?;
    if client.execute(
        "UPDATE post_build_hook_roots SET retained = true
         WHERE attempt_id = $1 AND output_index = $2 AND store_path = $3 AND released_at IS NULL",
        &[&root.attempt_id, &root.output_index, &root.store_path],
    )? != 1
    {
        return Err("hook delivery root is unavailable".into());
    }
    Ok(())
}

pub fn read_pending_post_build_hook_roots(
    database: &(impl DatabaseSource + ?Sized),
    after: Option<(i64, i32)>,
) -> Result<Vec<PostBuildHookRoot>, Box<dyn std::error::Error + Send + Sync>> {
    let (attempt_id, output_index) = after.unwrap_or((0, -1));
    connect(database)?
        .query(
            "SELECT r.attempt_id, r.output_index, r.store_path
         FROM post_build_hook_roots r JOIN post_build_hooks_outbox p USING (attempt_id)
         WHERE p.state = 'pending' AND r.released_at IS NULL
           AND (r.attempt_id, r.output_index) > ($1, $2)
         ORDER BY r.attempt_id, r.output_index LIMIT 256",
            &[&attempt_id, &output_index],
        )?
        .iter()
        .map(|row| {
            Ok(PostBuildHookRoot {
                attempt_id: row.try_get(0)?,
                output_index: row.try_get(1)?,
                store_path: row.try_get(2)?,
            })
        })
        .collect()
}

pub fn read_post_build_hook_root_cleanup(
    database: &(impl DatabaseSource + ?Sized),
    limit: usize,
) -> Result<Vec<PostBuildHookRoot>, Box<dyn std::error::Error + Send + Sync>> {
    if !(1..=256).contains(&limit) {
        return Err("invalid hook delivery cleanup limit".into());
    }
    let mut client = connect(database)?;
    client
        .query(
            "SELECT r.attempt_id, r.output_index, r.store_path
         FROM post_build_hook_roots r
         JOIN shared_build_attempts a USING (attempt_id)
         LEFT JOIN post_build_hooks_outbox p USING (attempt_id)
         WHERE r.released_at IS NULL
           AND (a.state = 'failed' OR p.state IN ('succeeded', 'abandoned')
                OR (a.state = 'succeeded' AND p.attempt_id IS NULL))
         ORDER BY r.attempt_id, r.output_index LIMIT $1",
            &[&(limit as i64)],
        )?
        .iter()
        .map(|row| {
            Ok(PostBuildHookRoot {
                attempt_id: row.try_get(0)?,
                output_index: row.try_get(1)?,
                store_path: row.try_get(2)?,
            })
        })
        .collect()
}

pub fn release_post_build_hook_root(
    database: &(impl DatabaseSource + ?Sized),
    root: &PostBuildHookRoot,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let mut client = connect(database)?;
    Ok(client.execute(
        "UPDATE post_build_hook_roots r SET released_at = clock_timestamp()
         FROM shared_build_attempts a
         WHERE r.attempt_id = a.attempt_id AND r.attempt_id = $1 AND r.output_index = $2
           AND r.released_at IS NULL
           AND (a.state = 'failed' OR EXISTS (
               SELECT 1 FROM post_build_hooks_outbox p WHERE p.attempt_id = r.attempt_id
               AND p.state IN ('succeeded', 'abandoned')
           ) OR (a.state = 'succeeded' AND NOT EXISTS (
               SELECT 1 FROM post_build_hooks_outbox p WHERE p.attempt_id = r.attempt_id
           )))",
        &[&root.attempt_id, &root.output_index],
    )? == 1)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookDelivery {
    pub attempt_id: i64,
    pub output_paths: Vec<String>,
    pub attempt_count: i32,
    pub claim_token: String,
    pub age: Duration,
}

pub fn claim_post_build_hooks(
    database: &(impl DatabaseSource + ?Sized),
    claim_token: &str,
    claim_duration: Duration,
) -> Result<Option<HookDelivery>, Box<dyn std::error::Error + Send + Sync>> {
    if claim_token.is_empty() || claim_token.len() > 256 || claim_duration.is_zero() {
        return Err("invalid cache hook delivery claim".into());
    }
    let seconds = claim_duration.as_secs_f64();
    let mut client = connect(database)?;
    let row = client.query_opt(
        "UPDATE post_build_hooks_outbox
         SET claim_token = $1,
             claim_expires_at = clock_timestamp() + make_interval(secs => $2),
             attempt_count = attempt_count + 1
         WHERE attempt_id = (
             SELECT attempt_id FROM post_build_hooks_outbox
             WHERE state = 'pending' AND next_attempt_at <= clock_timestamp()
               AND (claim_expires_at IS NULL OR claim_expires_at <= clock_timestamp())
             ORDER BY next_attempt_at, attempt_id
             LIMIT 1 FOR UPDATE SKIP LOCKED
         )
         RETURNING attempt_id, output_paths, attempt_count,
                   GREATEST(0, extract(epoch FROM (clock_timestamp() - created_at)))::double precision",
        &[&claim_token, &seconds],
    )?;
    row.map(|row| {
        Ok(HookDelivery {
            attempt_id: row.try_get(0)?,
            output_paths: row.try_get(1)?,
            attempt_count: row.try_get(2)?,
            claim_token: claim_token.to_owned(),
            age: Duration::from_secs_f64(row.try_get(3)?),
        })
    })
    .transpose()
}

pub fn acknowledge_post_build_hooks(
    database: &(impl DatabaseSource + ?Sized),
    hook_delivery: &HookDelivery,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let mut client = connect(database)?;
    Ok(client.execute(
        "UPDATE post_build_hooks_outbox
         SET state = 'succeeded', completed_at = clock_timestamp(),
             claim_token = NULL, claim_expires_at = NULL, last_failure_class = NULL
         WHERE attempt_id = $1 AND claim_token = $2 AND state = 'pending'
           AND claim_expires_at > clock_timestamp()",
        &[&hook_delivery.attempt_id, &hook_delivery.claim_token],
    )? == 1)
}

pub fn prune_post_build_hooks(
    database: &(impl DatabaseSource + ?Sized),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut client = connect(database)?;
    let mut transaction = client.transaction()?;
    transaction.execute(
        "DELETE FROM post_build_hook_roots WHERE (attempt_id, output_index) IN (
             SELECT r.attempt_id, r.output_index FROM post_build_hook_roots r
             WHERE r.released_at IS NOT NULL AND NOT EXISTS (
                 SELECT 1 FROM shared_build_attempts a JOIN build_requests b USING (derivation_path)
                 WHERE a.attempt_id = r.attempt_id AND (
                     EXISTS (SELECT 1 FROM request_attachments t WHERE t.request_id = b.request_id)
                     OR EXISTS (SELECT 1 FROM store_leases l WHERE l.owner_kind = 'request' AND l.owner_id = b.request_id)
                 )
             ) ORDER BY r.attempt_id, r.output_index LIMIT 256
         )",
        &[],
    )?;
    transaction.execute(
        "DELETE FROM post_build_hooks_outbox WHERE attempt_id IN (
             SELECT p.attempt_id FROM post_build_hooks_outbox p
             WHERE p.state <> 'pending' AND NOT EXISTS (
                 SELECT 1 FROM post_build_hook_roots r WHERE r.attempt_id = p.attempt_id
             ) AND NOT EXISTS (
                 SELECT 1 FROM shared_build_attempts a JOIN build_requests b USING (derivation_path)
                 WHERE a.attempt_id = p.attempt_id AND (
                     EXISTS (SELECT 1 FROM request_attachments r WHERE r.request_id = b.request_id)
                     OR EXISTS (SELECT 1 FROM store_leases l WHERE l.owner_kind = 'request' AND l.owner_id = b.request_id)
                 )
             ) ORDER BY p.attempt_id LIMIT 256
         )",
        &[],
    )?;
    transaction.commit()?;
    Ok(())
}

pub fn post_build_hooks_backlog(
    database: &(impl DatabaseSource + ?Sized),
) -> Result<(u64, f64), Box<dyn std::error::Error + Send + Sync>> {
    let row = connect(database)?.query_one(
        "SELECT count(*), COALESCE(GREATEST(0, extract(epoch FROM (clock_timestamp() - min(created_at)))), 0)::double precision
         FROM post_build_hooks_outbox WHERE state = 'pending'", &[],
    )?;
    Ok((u64::try_from(row.try_get::<_, i64>(0)?)?, row.try_get(1)?))
}

pub fn abandon_disabled_post_build_hooks(
    database: &(impl DatabaseSource + ?Sized),
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    Ok(connect(database)?.execute(
        "UPDATE post_build_hooks_outbox SET state = 'abandoned',
             completed_at = clock_timestamp(), last_failure_class = 'hook-disabled',
             claim_token = NULL, claim_expires_at = NULL
         WHERE state = 'pending'",
        &[],
    )?)
}

pub fn abandon_missing_post_build_hooks(
    database: &(impl DatabaseSource + ?Sized),
    attempt_id: i64,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    Ok(connect(database)?.execute(
        "UPDATE post_build_hooks_outbox SET state = 'abandoned',
             completed_at = clock_timestamp(), last_failure_class = 'missing-path',
             claim_token = NULL, claim_expires_at = NULL
         WHERE attempt_id = $1 AND state = 'pending'
           AND (claim_expires_at IS NULL OR claim_expires_at <= clock_timestamp())",
        &[&attempt_id],
    )? == 1)
}

pub fn abandon_post_build_hooks(
    database: &(impl DatabaseSource + ?Sized),
    hook_delivery: &HookDelivery,
    failure_class: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    if failure_class.is_empty() || failure_class.len() > 64 {
        return Err("invalid hook delivery abandonment".into());
    }
    let mut client = connect(database)?;
    Ok(client.execute(
        "UPDATE post_build_hooks_outbox
         SET state = 'abandoned', completed_at = clock_timestamp(), last_failure_class = $3,
             claim_token = NULL, claim_expires_at = NULL
         WHERE attempt_id = $1 AND claim_token = $2 AND state = 'pending'
           AND claim_expires_at > clock_timestamp()",
        &[
            &hook_delivery.attempt_id,
            &hook_delivery.claim_token,
            &failure_class,
        ],
    )? == 1)
}

pub fn retry_post_build_hooks(
    database: &(impl DatabaseSource + ?Sized),
    hook_delivery: &HookDelivery,
    failure_class: &str,
    delay: Duration,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    if failure_class.is_empty() || failure_class.len() > 64 || delay.is_zero() {
        return Err("invalid cache hook delivery retry".into());
    }
    let mut client = connect(database)?;
    let seconds = delay.as_secs_f64();
    Ok(client.execute(
        "UPDATE post_build_hooks_outbox
         SET next_attempt_at = clock_timestamp() + make_interval(secs => $3),
             last_failure_class = $4, claim_token = NULL, claim_expires_at = NULL
         WHERE attempt_id = $1 AND claim_token = $2 AND state = 'pending'
           AND claim_expires_at > clock_timestamp()",
        &[
            &hook_delivery.attempt_id,
            &hook_delivery.claim_token,
            &seconds,
            &failure_class,
        ],
    )? == 1)
}
