CREATE TABLE post_build_hooks_outbox (
    attempt_id bigint PRIMARY KEY REFERENCES shared_build_attempts(attempt_id),
    output_paths text[] NOT NULL CHECK (cardinality(output_paths) BETWEEN 1 AND 16),
    state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'succeeded', 'abandoned')),
    attempt_count integer NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    next_attempt_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    claim_token text,
    claim_expires_at timestamptz,
    last_failure_class text CHECK (length(last_failure_class) BETWEEN 1 AND 64),
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    completed_at timestamptz,
    roots_released_at timestamptz,
    CHECK ((claim_token IS NULL) = (claim_expires_at IS NULL)),
    CHECK ((state = 'pending') = (completed_at IS NULL)),
    CHECK (roots_released_at IS NULL OR state <> 'pending')
);

CREATE TABLE post_build_hook_roots (
    attempt_id bigint NOT NULL REFERENCES shared_build_attempts(attempt_id),
    output_index integer NOT NULL CHECK (output_index BETWEEN 0 AND 15),
    store_path text NOT NULL CHECK (octet_length(store_path) BETWEEN 1 AND 4096),
    retained boolean NOT NULL DEFAULT false,
    released_at timestamptz,
    PRIMARY KEY (attempt_id, output_index)
);

CREATE TRIGGER singleton_ownership_fence
    BEFORE INSERT OR UPDATE OR DELETE ON post_build_hook_roots
    FOR EACH STATEMENT EXECUTE FUNCTION enforce_singleton_ownership();

CREATE INDEX post_build_hooks_due_idx ON post_build_hooks_outbox (next_attempt_at, attempt_id)
    WHERE state = 'pending';
CREATE INDEX post_build_hooks_cleanup_idx ON post_build_hooks_outbox (attempt_id)
    WHERE state <> 'pending' AND roots_released_at IS NULL;

CREATE TRIGGER singleton_ownership_fence
    BEFORE INSERT OR UPDATE OR DELETE ON post_build_hooks_outbox
    FOR EACH STATEMENT EXECUTE FUNCTION enforce_singleton_ownership();
