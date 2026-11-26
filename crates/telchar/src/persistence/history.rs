//! Archives eligible terminal history before deleting its durable records.

use std::io;

use super::*;

#[derive(Default)]
pub struct HistoryCursor {
    after: Option<String>,
}

pub fn archive_terminal_history(
    database: &(impl DatabaseSource + ?Sized),
    retention: Duration,
    maximum_rows: usize,
    deliver: impl FnMut(&str, &[u8]) -> io::Result<()>,
) -> io::Result<u64> {
    sweep_terminal_history(
        database,
        retention,
        maximum_rows,
        &mut HistoryCursor::default(),
        deliver,
    )
}

pub fn sweep_terminal_history(
    database: &(impl DatabaseSource + ?Sized),
    retention: Duration,
    maximum_rows: usize,
    cursor: &mut HistoryCursor,
    mut deliver: impl FnMut(&str, &[u8]) -> io::Result<()>,
) -> io::Result<u64> {
    if retention.is_zero()
        || retention.subsec_nanos() != 0
        || retention.as_secs() > 3650 * 86400
        || !(1..=256).contains(&maximum_rows)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid history retention bounds",
        ));
    }
    let mut client = connect(database).map_err(io::Error::other)?;
    let mut deleted = 0;
    for _ in 0..maximum_rows {
        let mut delivered: Option<(String, Vec<u8>)> = None;
        for phase in 0..2 {
            let mut transaction = client.transaction().map_err(io::Error::other)?;
            let seconds = retention.as_secs() as f64;
            let candidate = transaction.query_opt(
            "SELECT b.derivation_path FROM shared_builds b
             WHERE ($2::text IS NULL OR b.derivation_path = $2)
               AND ($2::text IS NOT NULL OR $3::text IS NULL OR b.derivation_path > $3)
               AND b.state IN ('succeeded', 'failed')
               AND b.completed_at < transaction_timestamp() - make_interval(secs => $1)
               AND (b.expires_at IS NULL OR b.expires_at < transaction_timestamp())
               AND NOT EXISTS (SELECT 1 FROM build_requests r WHERE r.derivation_path = b.derivation_path
                   AND (EXISTS (SELECT 1 FROM request_attachments x WHERE x.request_id = r.request_id)
                     OR EXISTS (SELECT 1 FROM store_leases l WHERE l.owner_kind = 'request' AND l.owner_id = r.request_id)))
               AND NOT EXISTS (SELECT 1 FROM shared_build_attempts a
                   WHERE a.derivation_path = b.derivation_path AND (
                     a.state NOT IN ('succeeded', 'failed')
                     OR EXISTS (SELECT 1 FROM attempt_log_archives l WHERE l.attempt_id = a.attempt_id AND (l.state IN ('capturing', 'pending') OR l.spool_name IS NOT NULL))
                     OR a.completed_at >= transaction_timestamp() - make_interval(secs => $1)
                     OR EXISTS (SELECT 1 FROM nomad_execution_obligations n WHERE n.attempt_id = a.attempt_id AND n.resolved_at IS NULL)
                     OR EXISTS (SELECT 1 FROM post_build_hooks_outbox h WHERE h.attempt_id = a.attempt_id AND (h.state = 'pending' OR h.roots_released_at IS NULL))
                     OR EXISTS (SELECT 1 FROM post_build_hook_roots h WHERE h.attempt_id = a.attempt_id AND h.released_at IS NULL)
                     OR EXISTS (SELECT 1 FROM microvm_launches m WHERE m.attempt_id = a.attempt_id)
                     OR EXISTS (SELECT 1 FROM microvm_workers m WHERE m.attempt_id = a.attempt_id)
                     OR EXISTS (SELECT 1 FROM microvm_assignments m WHERE m.attempt_id = a.attempt_id)))
             ORDER BY b.derivation_path LIMIT 1 FOR UPDATE OF b SKIP LOCKED",
            &[&seconds, &delivered.as_ref().map(|(path, _)| path), &cursor.after],
        ).map_err(io::Error::other)?;
            let Some(candidate) = candidate else {
                if phase == 0 {
                    cursor.after = None;
                    return Ok(deleted);
                }
                break;
            };
            let path: String = candidate.get(0);
            if phase == 0 {
                cursor.after = Some(path.clone());
            }
            // Request locks serialize admission and detail maintenance with deletion.
            transaction.query(
            "SELECT request_id FROM build_requests WHERE derivation_path = $1 ORDER BY request_id LIMIT 257 FOR UPDATE",
            &[&path],
        ).map_err(io::Error::other)?;
            let protected = transaction.query_one(
            "SELECT EXISTS (SELECT 1 FROM build_requests r WHERE r.derivation_path = $1
                AND (EXISTS (SELECT 1 FROM request_attachments x WHERE x.request_id = r.request_id)
                  OR EXISTS (SELECT 1 FROM store_leases l WHERE l.owner_kind = 'request' AND l.owner_id = r.request_id)))",
            &[&path],
        ).map_err(io::Error::other)?.get::<_, bool>(0);
            if protected {
                break;
            }
            for query in [
                "SELECT attempt_id FROM shared_build_attempts WHERE derivation_path = $1 ORDER BY attempt_id LIMIT 257 FOR UPDATE",
                "SELECT n.attempt_id FROM nomad_execution_obligations n JOIN shared_build_attempts a USING (attempt_id) WHERE derivation_path = $1 ORDER BY n.attempt_id LIMIT 257 FOR UPDATE OF n",
                "SELECT h.attempt_id FROM post_build_hooks_outbox h JOIN shared_build_attempts a USING (attempt_id) WHERE derivation_path = $1 ORDER BY h.attempt_id LIMIT 257 FOR UPDATE OF h",
                "SELECT h.attempt_id FROM post_build_hook_roots h JOIN shared_build_attempts a USING (attempt_id) WHERE derivation_path = $1 ORDER BY h.attempt_id, h.output_index LIMIT 257 FOR UPDATE OF h",
                "SELECT l.attempt_id FROM attempt_log_archives l JOIN shared_build_attempts a USING (attempt_id) WHERE derivation_path = $1 ORDER BY l.attempt_id LIMIT 257 FOR UPDATE OF l",
            ] {
                transaction
                    .query(query, &[&path])
                    .map_err(io::Error::other)?;
            }
            let pending: bool = transaction.query_one(
                "SELECT EXISTS (SELECT 1 FROM shared_build_attempts a WHERE derivation_path = $1 AND (
                    EXISTS (SELECT 1 FROM nomad_execution_obligations n WHERE n.attempt_id = a.attempt_id AND n.resolved_at IS NULL)
                    OR EXISTS (SELECT 1 FROM attempt_log_archives l WHERE l.attempt_id = a.attempt_id AND (l.state IN ('capturing', 'pending') OR l.spool_name IS NOT NULL))
                    OR EXISTS (SELECT 1 FROM post_build_hooks_outbox h WHERE h.attempt_id = a.attempt_id AND (h.state = 'pending' OR h.roots_released_at IS NULL))
                    OR EXISTS (SELECT 1 FROM post_build_hook_roots h WHERE h.attempt_id = a.attempt_id AND h.released_at IS NULL)))",
                &[&path],
            ).map_err(io::Error::other)?.get(0);
            if pending {
                break;
            }
            let payload = history_snapshot(&mut transaction, &path)?;
            let key = format!(
                "history/{}.jsonl",
                Sha256::digest(&payload)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            if phase == 0 {
                transaction.commit().map_err(io::Error::other)?;
                deliver(&key, &payload)?;
                delivered = Some((path, payload));
                continue;
            }
            if delivered.as_ref().map(|(_, bytes)| bytes) != Some(&payload) {
                break;
            }
            for sql in [
                "DELETE FROM attempt_log_archives WHERE attempt_id IN (SELECT attempt_id FROM shared_build_attempts WHERE derivation_path = $1)",
                "DELETE FROM post_build_hook_roots WHERE attempt_id IN (SELECT attempt_id FROM shared_build_attempts WHERE derivation_path = $1)",
                "DELETE FROM post_build_hooks_outbox WHERE attempt_id IN (SELECT attempt_id FROM shared_build_attempts WHERE derivation_path = $1)",
                "DELETE FROM nomad_execution_obligations WHERE attempt_id IN (SELECT attempt_id FROM shared_build_attempts WHERE derivation_path = $1)",
                "DELETE FROM shared_build_attempt_outcomes WHERE attempt_id IN (SELECT attempt_id FROM shared_build_attempts WHERE derivation_path = $1)",
                "DELETE FROM shared_build_attempts WHERE derivation_path = $1",
                "DELETE FROM build_requests WHERE derivation_path = $1",
                "DELETE FROM shared_builds WHERE derivation_path = $1",
            ] {
                transaction
                    .execute(sql, &[&path])
                    .map_err(io::Error::other)?;
            }
            transaction.commit().map_err(io::Error::other)?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

fn history_snapshot(transaction: &mut DatabaseTransaction<'_>, path: &str) -> io::Result<Vec<u8>> {
    const MAXIMUM_RECORDS: i64 = 256;
    const MAXIMUM_BYTES: usize = 8 * 1024 * 1024;
    let build: Option<String> = transaction
        .query_one(
            "SELECT CASE WHEN octet_length(row_to_json(record)::text) < 8388608 THEN row_to_json(record)::text ELSE NULL END FROM (SELECT derivation_path, request_digest, state, backend_name, backend_kind, execution_recovery, cancellation, log_recovery, backend_execution_id, expected_outputs, result_metadata, failure_classification, created_at, started_at, collecting_at, completed_at, expires_at, quota_subject, queue_position, queued_at FROM shared_builds WHERE derivation_path = $1) record",
            &[&path],
        )
        .map_err(io::Error::other)?
        .get(0);
    let build = build.ok_or_else(|| io::Error::other("history snapshot byte limit exceeded"))?;
    let mut document = serde_json::json!({
        "schema_version": 1,
        "build": serde_json::from_str::<serde_json::Value>(&build).map_err(io::Error::other)?,
    });
    let mut bytes = build.len();
    for (name, query) in [
        (
            "logs",
            "SELECT row_to_json(record)::text FROM (SELECT l.attempt_id, l.state, l.complete, l.uncompressed_bytes, l.object_key, l.sha256, l.created_at, l.updated_at FROM attempt_log_archives l JOIN shared_build_attempts a USING (attempt_id) WHERE derivation_path = $1 ORDER BY l.attempt_id LIMIT $2) record",
        ),
        (
            "attempts",
            "SELECT row_to_json(record)::text FROM (SELECT attempt_id, derivation_path, ordinal, backend_name, backend_kind, backend_execution_id, state, created_at, started_at, collecting_at, completed_at, traceparent, tracestate FROM shared_build_attempts WHERE derivation_path = $1 ORDER BY ordinal LIMIT $2) record",
        ),
        (
            "outcomes",
            "SELECT row_to_json(record)::text FROM (SELECT o.attempt_id, o.classification, o.result_metadata, o.created_at FROM shared_build_attempt_outcomes o JOIN shared_build_attempts a USING (attempt_id) WHERE derivation_path = $1 ORDER BY o.attempt_id LIMIT $2) record",
        ),
        (
            "requests",
            "SELECT row_to_json(record)::text FROM (SELECT request_id, derivation_path, system, created_at, audit_subject, quota_subject FROM build_requests WHERE derivation_path = $1 ORDER BY request_id LIMIT $2) record",
        ),
        (
            "obligations",
            "SELECT row_to_json(record)::text FROM (SELECT n.attempt_id, n.created_at, n.resolved_at FROM nomad_execution_obligations n JOIN shared_build_attempts a USING (attempt_id) WHERE derivation_path = $1 ORDER BY n.attempt_id LIMIT $2) record",
        ),
        (
            "hooks",
            "SELECT row_to_json(record)::text FROM (SELECT h.attempt_id, h.output_paths, h.state, h.attempt_count, h.next_attempt_at, h.last_failure_class, h.created_at, h.completed_at, h.roots_released_at FROM post_build_hooks_outbox h JOIN shared_build_attempts a USING (attempt_id) WHERE derivation_path = $1 ORDER BY h.attempt_id LIMIT $2) record",
        ),
        (
            "hook_roots",
            "SELECT row_to_json(record)::text FROM (SELECT h.attempt_id, h.output_index, h.store_path, h.retained, h.released_at FROM post_build_hook_roots h JOIN shared_build_attempts a USING (attempt_id) WHERE derivation_path = $1 ORDER BY h.attempt_id, h.output_index LIMIT $2) record",
        ),
    ] {
        let bounded_query = format!(
            "SELECT CASE WHEN sum(octet_length(value)) OVER (ROWS UNBOUNDED PRECEDING) <= $3::bigint THEN value ELSE NULL END FROM ({query}) page(value)"
        );
        let remaining = MAXIMUM_BYTES.saturating_sub(bytes) as i64;
        let rows = transaction
            .query(&bounded_query, &[&path, &(MAXIMUM_RECORDS + 1), &remaining])
            .map_err(io::Error::other)?;
        if rows.len() > MAXIMUM_RECORDS as usize {
            return Err(io::Error::other("history snapshot record limit exceeded"));
        }
        let mut records = Vec::new();
        for row in rows {
            let record: Option<String> = row.get(0);
            let record =
                record.ok_or_else(|| io::Error::other("history snapshot byte limit exceeded"))?;
            bytes += record.len();
            if bytes > MAXIMUM_BYTES {
                return Err(io::Error::other("history snapshot byte limit exceeded"));
            }
            records.push(
                serde_json::from_str::<serde_json::Value>(&record).map_err(io::Error::other)?,
            );
        }
        document[name] = serde_json::Value::Array(records);
    }
    let mut payload = serde_json::to_vec(&document).map_err(io::Error::other)?;
    if payload.len() >= MAXIMUM_BYTES {
        return Err(io::Error::other("history snapshot byte limit exceeded"));
    }
    payload.push(b'\n');
    Ok(payload)
}
