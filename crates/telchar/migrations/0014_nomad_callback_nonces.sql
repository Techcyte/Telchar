-- Adds replay-resistant Nomad callback nonce reservations.
CREATE TABLE nomad_callback_nonces (
    nonce_digest bytea PRIMARY KEY CHECK (octet_length(nonce_digest) = 32),
    backend_name text NOT NULL CHECK (backend_name <> '' AND octet_length(backend_name) <= 256),
    job_id text NOT NULL CHECK (job_id <> '' AND octet_length(job_id) <= 256),
    allocation_id text NOT NULL CHECK (allocation_id <> '' AND octet_length(allocation_id) <= 256),
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at)
);

CREATE INDEX nomad_callback_nonces_expiry_idx
    ON nomad_callback_nonces (expires_at);
