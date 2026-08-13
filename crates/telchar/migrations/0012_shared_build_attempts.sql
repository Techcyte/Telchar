-- Adds durable backend attempt identities and terminal attempt outcomes.
CREATE TABLE shared_build_attempts (
    attempt_id bigserial PRIMARY KEY,
    derivation_path text NOT NULL REFERENCES shared_builds(derivation_path),
    ordinal integer NOT NULL CONSTRAINT shared_build_attempts_ordinal_check CHECK (ordinal > 0),
    backend_name text NOT NULL CONSTRAINT shared_build_attempts_backend_name_check CHECK (
        length(backend_name) BETWEEN 1 AND 256
    ),
    backend_kind text NOT NULL CONSTRAINT shared_build_attempts_backend_kind_check CHECK (
        backend_kind IN ('local', 'static-ssh', 'nomad')
    ),
    backend_execution_id text CONSTRAINT shared_build_attempts_backend_execution_id_check CHECK (
        backend_execution_id IS NULL OR length(backend_execution_id) BETWEEN 1 AND 4096
    ),
    state text NOT NULL CONSTRAINT shared_build_attempts_state_check CHECK (
        state IN ('running', 'collecting', 'succeeded', 'failed')
    ),
    created_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz NOT NULL DEFAULT now(),
    collecting_at timestamptz,
    completed_at timestamptz,
    UNIQUE (derivation_path, ordinal),
    CONSTRAINT shared_build_attempts_timestamp_order_check CHECK (
        started_at >= created_at
        AND (collecting_at IS NULL OR collecting_at >= started_at)
        AND (completed_at IS NULL OR completed_at >= COALESCE(collecting_at, started_at))
    ),
    CONSTRAINT shared_build_attempts_state_timestamp_check CHECK (
        (state = 'running' AND collecting_at IS NULL AND completed_at IS NULL)
        OR (state = 'collecting' AND collecting_at IS NOT NULL AND completed_at IS NULL)
        OR (state IN ('succeeded', 'failed') AND completed_at IS NOT NULL)
    )
);

INSERT INTO shared_build_attempts (
    derivation_path, ordinal, backend_name, backend_kind, backend_execution_id,
    state, created_at, started_at, collecting_at
)
SELECT derivation_path, 1, backend_name, backend_kind, backend_execution_id,
       state, created_at, started_at, collecting_at
FROM shared_builds
WHERE state IN ('running', 'collecting');

CREATE UNIQUE INDEX shared_build_attempts_one_active_idx
    ON shared_build_attempts (derivation_path)
    WHERE state IN ('running', 'collecting');

CREATE INDEX shared_build_attempts_active_started_idx
    ON shared_build_attempts (started_at, attempt_id)
    WHERE state IN ('running', 'collecting');

CREATE TABLE shared_build_attempt_outcomes (
    attempt_id bigint PRIMARY KEY REFERENCES shared_build_attempts(attempt_id),
    classification text NOT NULL CONSTRAINT shared_build_attempt_outcomes_classification_check CHECK (
        length(classification) BETWEEN 1 AND 256
    ),
    result_metadata jsonb NOT NULL CONSTRAINT shared_build_attempt_outcomes_result_metadata_check CHECK (
        jsonb_typeof(result_metadata) = 'object'
        AND octet_length(result_metadata::text) <= 1048576
    ),
    created_at timestamptz NOT NULL DEFAULT now()
);
