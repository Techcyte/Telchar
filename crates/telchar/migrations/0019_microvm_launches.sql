CREATE TABLE microvm_launches (
    intent_id text PRIMARY KEY CHECK (length(intent_id) BETWEEN 1 AND 128),
    attempt_id bigint NOT NULL UNIQUE REFERENCES shared_build_attempts(attempt_id),
    backend_name text NOT NULL CHECK (length(backend_name) BETWEEN 1 AND 256),
    account_id text NOT NULL CHECK (account_id ~ '^[0-9]{12}$'),
    region text NOT NULL CHECK (length(region) BETWEEN 1 AND 64),
    image_arn text NOT NULL CHECK (length(image_arn) BETWEEN 1 AND 2048),
    image_version text NOT NULL CHECK (length(image_version) BETWEEN 1 AND 128),
    configuration_digest bytea NOT NULL CHECK (octet_length(configuration_digest) = 32),
    request_digest bytea NOT NULL CHECK (octet_length(request_digest) = 32),
    client_token text NOT NULL UNIQUE CHECK (length(client_token) BETWEEN 1 AND 128),
    credential_verifier bytea NOT NULL CHECK (octet_length(credential_verifier) = 32),
    admission_cutoff timestamptz NOT NULL,
    maximum_duration_seconds integer NOT NULL CHECK (maximum_duration_seconds BETWEEN 1 AND 28800),
    phase text NOT NULL DEFAULT 'prepared' CHECK (phase IN ('prepared', 'dispatching', 'bound', 'ambiguous', 'closed')),
    provider_id text CHECK (length(provider_id) BETWEEN 1 AND 256),
    session_consumed_at timestamptz,
    reservation_released_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (admission_cutoff > created_at)
);

CREATE INDEX microvm_launches_reserved_backend_idx
    ON microvm_launches (backend_name, created_at)
    WHERE reservation_released_at IS NULL;

CREATE TRIGGER singleton_ownership_fence
    BEFORE INSERT OR UPDATE OR DELETE ON microvm_launches
    FOR EACH STATEMENT EXECUTE FUNCTION enforce_singleton_ownership();
