use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadCallbackNonceError;

impl fmt::Display for NomadCallbackNonceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Nomad callback replay persistence failed")
    }
}

impl std::error::Error for NomadCallbackNonceError {}

fn valid_nomad_callback_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub fn reserve_nomad_callback_nonce(
    database_url: &str,
    backend_name: &str,
    job_id: &str,
    allocation_id: &str,
    nonce: &str,
    expires_at: SystemTime,
    maximum_retained_nonces: usize,
) -> Result<bool, NomadCallbackNonceError> {
    if database_url.trim().is_empty()
        || !valid_nomad_callback_component(backend_name)
        || !valid_nomad_callback_component(job_id)
        || !valid_nomad_callback_component(allocation_id)
        || nonce.is_empty()
        || nonce.len() > 256
        || nonce.chars().any(char::is_control)
        || maximum_retained_nonces == 0
    {
        return Err(NomadCallbackNonceError);
    }
    let maximum_retained_nonces =
        i64::try_from(maximum_retained_nonces).map_err(|_| NomadCallbackNonceError)?;
    let nonce_digest = Sha256::digest(nonce.as_bytes()).to_vec();
    let mut client = Client::connect(database_url, NoTls).map_err(|_| NomadCallbackNonceError)?;
    let mut transaction = client.transaction().map_err(|_| NomadCallbackNonceError)?;
    transaction
        .query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended('nomad_callback_nonces', 0))",
            &[],
        )
        .map_err(|_| NomadCallbackNonceError)?;
    transaction
        .execute(
            "DELETE FROM nomad_callback_nonces WHERE expires_at <= transaction_timestamp()",
            &[],
        )
        .map_err(|_| NomadCallbackNonceError)?;
    let retained: i64 = transaction
        .query_one("SELECT count(*) FROM nomad_callback_nonces", &[])
        .map_err(|_| NomadCallbackNonceError)?
        .try_get(0)
        .map_err(|_| NomadCallbackNonceError)?;
    if retained >= maximum_retained_nonces {
        return Err(NomadCallbackNonceError);
    }
    let inserted = transaction
        .execute(
            "INSERT INTO nomad_callback_nonces (nonce_digest, backend_name, job_id, allocation_id, expires_at) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (nonce_digest) DO NOTHING",
            &[&nonce_digest, &backend_name, &job_id, &allocation_id, &expires_at],
        )
        .map_err(|_| NomadCallbackNonceError)?;
    transaction.commit().map_err(|_| NomadCallbackNonceError)?;
    Ok(inserted == 1)
}
