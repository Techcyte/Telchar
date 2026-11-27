//! Persists attempt-scoped archive publication state without log bodies.

use super::*;

pub fn read_attempt_log_archive(
    database: &(impl DatabaseSource + ?Sized),
    derivation_path: &str,
) -> Result<Option<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(connect(database)?
        .query_opt(
            "SELECT jsonb_build_object('attempt_id', l.attempt_id, 'state', l.state,
            'complete', l.complete, 'uncompressed_bytes', l.uncompressed_bytes,
            'object_key', l.object_key, 'sha256', l.sha256)::text
         FROM attempt_log_archives l JOIN shared_build_attempts a USING (attempt_id)
         WHERE a.derivation_path = $1 AND a.ordinal = (
             SELECT max(ordinal) FROM shared_build_attempts WHERE derivation_path = $1
         )",
            &[&derivation_path],
        )?
        .map(|row| serde_json::from_str(&row.get::<_, String>(0)))
        .transpose()?)
}

pub fn begin_log_archive(
    database: &(impl DatabaseSource + ?Sized),
    attempt_id: i64,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let mut client = connect(database)?;
    Ok(client.execute(
        "INSERT INTO attempt_log_archives (attempt_id, state) VALUES ($1, 'capturing') ON CONFLICT DO NOTHING",
        &[&attempt_id],
    )? == 1)
}

pub fn fail_log_archive(
    database: &(impl DatabaseSource + ?Sized),
    attempt_id: i64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    connect(database)?.execute(
        "UPDATE attempt_log_archives SET state = 'failed', complete = false, updated_at = transaction_timestamp() WHERE attempt_id = $1 AND state = 'capturing'",
        &[&attempt_id],
    )?;
    Ok(())
}

pub fn stage_log_archive(
    database: &(impl DatabaseSource + ?Sized),
    attempt_id: i64,
    complete: bool,
    bytes: i64,
    spool_name: &str,
    checksum: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if spool_name != format!("{attempt_id}.log.zst") || attempt_id <= 0 {
        return Err(std::io::Error::other("invalid log spool name").into());
    }
    let updated = connect(database)?.execute(
        "UPDATE attempt_log_archives SET state = 'pending', complete = $2, uncompressed_bytes = $3,
         spool_name = $4, sha256 = $5, updated_at = transaction_timestamp() WHERE attempt_id = $1 AND state = 'capturing'",
        &[&attempt_id, &complete, &bytes, &spool_name, &checksum],
    )?;
    if updated != 1 {
        return Err(std::io::Error::other("log archive staging unavailable").into());
    }
    Ok(())
}

pub fn interrupted_log_archives(
    database: &(impl DatabaseSource + ?Sized),
    after: i64,
) -> Result<Vec<i64>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(connect(database)?.query("SELECT attempt_id FROM attempt_log_archives WHERE state = 'capturing' AND attempt_id > $1 ORDER BY attempt_id LIMIT 16", &[&after])?
        .iter().map(|row| row.get(0)).collect())
}

pub struct PendingLogArchive {
    pub attempt_id: i64,
    pub spool_name: String,
    pub complete: bool,
    pub uncompressed_bytes: i64,
    pub sha256: String,
}

pub fn pending_log_archives(
    database: &(impl DatabaseSource + ?Sized),
    after: i64,
) -> Result<Vec<PendingLogArchive>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(connect(database)?.query(
        "SELECT attempt_id, spool_name, complete, uncompressed_bytes, sha256 FROM attempt_log_archives WHERE state = 'pending' AND spool_name IS NOT NULL AND attempt_id > $1 ORDER BY attempt_id LIMIT 16", &[&after],
    )?.iter().map(|row| PendingLogArchive { attempt_id: row.get(0), spool_name: row.get(1), complete: row.get(2), uncompressed_bytes: row.get(3), sha256: row.get(4) }).collect())
}

pub struct AcknowledgedLogSpool {
    pub attempt_id: i64,
    pub spool_name: String,
    pub object_key: String,
}

pub fn release_log_spool(
    database: &(impl DatabaseSource + ?Sized),
    attempt_id: i64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    connect(database)?.execute(
        "UPDATE attempt_log_archives SET spool_name = NULL WHERE attempt_id = $1 AND state IN ('available', 'expired')",
        &[&attempt_id],
    )?;
    Ok(())
}

pub fn expire_local_log_archives(
    database: &(impl DatabaseSource + ?Sized),
    retention_days: i64,
    root: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !(1..=3650).contains(&retention_days) {
        return Err(std::io::Error::other("invalid log retention age").into());
    }
    connect(database)?.execute(
        "UPDATE attempt_log_archives SET state = 'expired', updated_at = transaction_timestamp()
         WHERE attempt_id IN (
             SELECT l.attempt_id FROM attempt_log_archives l JOIN shared_build_attempts a USING (attempt_id)
             WHERE l.state = 'available' AND a.state IN ('succeeded', 'failed')
               AND l.created_at < transaction_timestamp() - $1::bigint * interval '1 day'
               AND l.object_key = $2 || '/' || l.spool_name
             ORDER BY l.attempt_id LIMIT 16
         )", &[&retention_days, &root],
    )?;
    Ok(())
}

pub fn acknowledged_log_spools(
    database: &(impl DatabaseSource + ?Sized),
    after: i64,
) -> Result<Vec<AcknowledgedLogSpool>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(connect(database)?.query(
        "SELECT attempt_id, spool_name, CASE WHEN state = 'expired' THEN '' ELSE object_key END FROM attempt_log_archives WHERE state IN ('available', 'expired') AND spool_name IS NOT NULL AND attempt_id > $1 ORDER BY attempt_id LIMIT 16", &[&after],
    )?.iter().map(|row| AcknowledgedLogSpool { attempt_id: row.get(0), spool_name: row.get(1), object_key: row.get(2) }).collect())
}

pub fn finish_log_archive(
    database: &(impl DatabaseSource + ?Sized),
    attempt_id: i64,
    complete: bool,
    bytes: i64,
    key: &str,
    checksum: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let updated = connect(database)?.execute(
        "UPDATE attempt_log_archives SET state = 'available', complete = $2, uncompressed_bytes = $3,
         object_key = $4, sha256 = $5, updated_at = transaction_timestamp() WHERE attempt_id = $1 AND state IN ('capturing', 'pending')",
        &[&attempt_id, &complete, &bytes, &key, &checksum],
    )?;
    if updated != 1 {
        return Err(std::io::Error::other("log archive acknowledgement unavailable").into());
    }
    Ok(())
}
