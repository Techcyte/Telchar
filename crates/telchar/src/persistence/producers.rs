use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputProducer {
    pub backend_name: String,
    pub backend_kind: BackendKind,
    pub location: Option<String>,
    /// Advisory weight from the current retained input closure, never persisted.
    pub nar_size: u64,
}

pub fn record_attempt_producer_location(
    database: &(impl DatabaseSource + ?Sized),
    derivation_path: &str,
    attempt_id: i64,
    location: &str,
) -> std::io::Result<()> {
    if location.is_empty() || location.len() > 4096 || location.contains('\0') {
        return Err(std::io::Error::other("invalid producer location"));
    }
    let mut client = connect(database).map_err(std::io::Error::other)?;
    let mut transaction = client.transaction().map_err(std::io::Error::other)?;
    transaction
        .query_opt(
            "SELECT 1 FROM shared_builds WHERE derivation_path = $1
         AND state IN ('running', 'collecting') FOR UPDATE",
            &[&derivation_path],
        )
        .map_err(std::io::Error::other)?
        .ok_or_else(|| std::io::Error::other("producer execution is not active"))?;
    let changed = transaction
        .execute(
            "UPDATE shared_build_attempts a SET producer_location = $3
         FROM shared_builds b WHERE a.derivation_path = $1 AND a.attempt_id = $2
           AND b.derivation_path = a.derivation_path
           AND a.state IN ('running', 'collecting') AND a.backend_kind = 'nomad'
           AND a.backend_name = b.backend_name AND a.backend_kind = b.backend_kind
           AND a.backend_execution_id = b.backend_execution_id
           AND (a.producer_location IS NULL OR a.producer_location = $3)",
            &[&derivation_path, &attempt_id, &location],
        )
        .map_err(std::io::Error::other)?;
    if changed != 1 {
        return Err(std::io::Error::other(
            "producer attempt does not match active execution",
        ));
    }
    transaction.commit().map_err(std::io::Error::other)
}

pub fn read_output_producers(
    database: &(impl DatabaseSource + ?Sized),
    inputs: &[crate::store::closure::ClosurePath],
) -> std::io::Result<Vec<OutputProducer>> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let sizes = inputs
        .iter()
        .map(|path| (path.store_path.as_str(), path.nar_size))
        .collect::<std::collections::BTreeMap<_, _>>();
    let paths = sizes.keys().copied().collect::<Vec<_>>();
    let mut client = connect(database).map_err(std::io::Error::other)?;
    client
        .query(
            "SELECT backend_name, backend_kind, location, store_path FROM output_producers
         WHERE store_path = ANY($1) ORDER BY store_path",
            &[&paths],
        )
        .map_err(std::io::Error::other)?
        .iter()
        .map(|row| {
            let kind: String = row.try_get(1).map_err(std::io::Error::other)?;
            let store_path: &str = row.try_get(3).map_err(std::io::Error::other)?;
            Ok(OutputProducer {
                backend_name: row.try_get(0).map_err(std::io::Error::other)?,
                backend_kind: match kind.as_str() {
                    "local" => BackendKind::Local,
                    "static-ssh" => BackendKind::StaticSsh,
                    "nomad" => BackendKind::Nomad,
                    _ => return Err(std::io::Error::other("invalid producer backend kind")),
                },
                location: row.try_get(2).map_err(std::io::Error::other)?,
                nar_size: sizes[store_path],
            })
        })
        .collect()
}
