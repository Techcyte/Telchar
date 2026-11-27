CREATE TABLE attempt_log_archives (
    attempt_id bigint PRIMARY KEY REFERENCES shared_build_attempts(attempt_id),
    state text NOT NULL CHECK (state IN ('capturing', 'pending', 'available', 'failed')),
    complete boolean NOT NULL DEFAULT false,
    uncompressed_bytes bigint NOT NULL DEFAULT 0 CHECK (uncompressed_bytes >= 0),
    object_key text CHECK (octet_length(object_key) BETWEEN 1 AND 4096),
    sha256 text CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    CHECK (state <> 'available' OR (object_key IS NOT NULL AND sha256 IS NOT NULL))
);

CREATE TRIGGER singleton_ownership_fence
    BEFORE INSERT OR UPDATE OR DELETE ON attempt_log_archives
    FOR EACH STATEMENT EXECUTE FUNCTION enforce_singleton_ownership();

CREATE INDEX attempt_log_archives_pending_idx ON attempt_log_archives (attempt_id)
    WHERE state IN ('capturing', 'pending');
