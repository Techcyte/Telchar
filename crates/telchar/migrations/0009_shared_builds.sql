CREATE TABLE shared_builds (
    derivation_path text PRIMARY KEY CONSTRAINT shared_builds_derivation_path_check CHECK (
        length(derivation_path) BETWEEN 1 AND 4096
    ),
    request_digest bytea NOT NULL CONSTRAINT shared_builds_request_digest_check CHECK (
        octet_length(request_digest) = 32
    ),
    state text NOT NULL DEFAULT 'claimed' CONSTRAINT shared_builds_state_check CHECK (
        state IN ('claimed', 'running', 'collecting', 'succeeded', 'failed')
    ),
    backend_name text NOT NULL CONSTRAINT shared_builds_backend_name_check CHECK (
        length(backend_name) BETWEEN 1 AND 256
    ),
    backend_kind text NOT NULL CONSTRAINT shared_builds_backend_kind_check CHECK (
        backend_kind IN ('local', 'static-ssh', 'nomad')
    ),
    execution_recovery text NOT NULL CONSTRAINT shared_builds_execution_recovery_check CHECK (
        execution_recovery IN ('output-only', 'adoptable')
    ),
    cancellation text NOT NULL CONSTRAINT shared_builds_cancellation_check CHECK (
        cancellation IN ('connection-bound', 'explicit')
    ),
    log_recovery text NOT NULL CONSTRAINT shared_builds_log_recovery_check CHECK (
        log_recovery IN ('live-only', 'replayable')
    ),
    backend_execution_id text CONSTRAINT shared_builds_backend_execution_id_check CHECK (
        backend_execution_id IS NULL OR length(backend_execution_id) BETWEEN 1 AND 4096
    ),
    expected_outputs text[] NOT NULL CONSTRAINT shared_builds_expected_outputs_check CHECK (
        cardinality(expected_outputs) BETWEEN 1 AND 64
        AND array_position(expected_outputs, NULL) IS NULL
    ),
    result_metadata jsonb CONSTRAINT shared_builds_result_metadata_check CHECK (
        result_metadata IS NULL
        OR (
            jsonb_typeof(result_metadata) = 'object'
            AND octet_length(result_metadata::text) <= 1048576
        )
    ),
    failure_classification text CONSTRAINT shared_builds_failure_classification_check CHECK (
        failure_classification IS NULL OR length(failure_classification) BETWEEN 1 AND 256
    ),
    created_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz,
    collecting_at timestamptz,
    completed_at timestamptz,
    expires_at timestamptz,
    CONSTRAINT shared_builds_adoptable_identity_check CHECK (
        execution_recovery != 'adoptable' OR backend_execution_id IS NOT NULL
    ),
    CONSTRAINT shared_builds_timestamp_order_check CHECK (
        (started_at IS NULL OR started_at >= created_at)
        AND (collecting_at IS NULL OR (started_at IS NOT NULL AND collecting_at >= started_at))
        AND (completed_at IS NULL OR completed_at >= COALESCE(collecting_at, started_at, created_at))
        AND (expires_at IS NULL OR (completed_at IS NOT NULL AND expires_at >= completed_at))
    ),
    CONSTRAINT shared_builds_state_timestamp_check CHECK (
        (state = 'claimed' AND started_at IS NULL AND collecting_at IS NULL AND completed_at IS NULL)
        OR (state = 'running' AND started_at IS NOT NULL AND collecting_at IS NULL AND completed_at IS NULL)
        OR (state = 'collecting' AND started_at IS NOT NULL AND collecting_at IS NOT NULL AND completed_at IS NULL)
        OR (state IN ('succeeded', 'failed') AND completed_at IS NOT NULL)
    ),
    CONSTRAINT shared_builds_terminal_result_check CHECK (
        (state = 'succeeded' AND result_metadata IS NOT NULL AND failure_classification IS NULL)
        OR (state = 'failed' AND result_metadata IS NOT NULL AND failure_classification IS NOT NULL)
        OR (state NOT IN ('succeeded', 'failed') AND result_metadata IS NULL AND failure_classification IS NULL AND expires_at IS NULL)
    )
);

CREATE INDEX shared_builds_active_created_idx
    ON shared_builds (created_at, derivation_path)
    WHERE state IN ('claimed', 'running', 'collecting');

CREATE INDEX shared_builds_terminal_expiry_idx
    ON shared_builds (expires_at, derivation_path)
    WHERE state IN ('succeeded', 'failed');
