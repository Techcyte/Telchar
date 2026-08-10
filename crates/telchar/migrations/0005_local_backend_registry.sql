CREATE TABLE local_backend_executions (
    backend_execution_id text PRIMARY KEY CONSTRAINT local_backend_executions_id_check CHECK (length(backend_execution_id) BETWEEN 1 AND 4096),
    idempotency_key text NOT NULL UNIQUE CONSTRAINT local_backend_executions_idempotency_key_check CHECK (length(idempotency_key) BETWEEN 1 AND 4096),
    specification_digest bytea NOT NULL CONSTRAINT local_backend_executions_specification_digest_check CHECK (octet_length(specification_digest) = 32),
    state text NOT NULL CONSTRAINT local_backend_executions_state_check CHECK (
        state IN ('accepted', 'running', 'succeeded', 'failed', 'cancelled')
    ),
    created_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz,
    completed_at timestamptz,
    CONSTRAINT local_backend_executions_timestamp_order_check CHECK (
        (started_at IS NULL OR started_at >= created_at)
        AND (completed_at IS NULL OR completed_at >= COALESCE(started_at, created_at))
    ),
    CONSTRAINT local_backend_executions_state_timestamp_check CHECK (
        (state = 'accepted' AND started_at IS NULL AND completed_at IS NULL)
        OR (state = 'running' AND started_at IS NOT NULL AND completed_at IS NULL)
        OR (state IN ('succeeded', 'failed', 'cancelled') AND completed_at IS NOT NULL)
    )
);
