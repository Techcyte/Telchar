CREATE TABLE nomad_execution_obligations (
    attempt_id bigint PRIMARY KEY REFERENCES shared_build_attempts(attempt_id),
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    resolved_at timestamptz,
    CONSTRAINT nomad_execution_obligations_resolution_order
        CHECK (resolved_at IS NULL OR resolved_at >= created_at)
);

CREATE INDEX nomad_execution_obligations_pending_idx
    ON nomad_execution_obligations (attempt_id) WHERE resolved_at IS NULL;

CREATE TRIGGER singleton_ownership_fence
    BEFORE INSERT OR UPDATE OR DELETE ON nomad_execution_obligations
    FOR EACH STATEMENT EXECUTE FUNCTION enforce_singleton_ownership();
