ALTER TABLE protocol_sessions
    ADD COLUMN audit_subject text NOT NULL DEFAULT 'gate-three',
    ADD COLUMN quota_subject text NOT NULL DEFAULT 'gate-three';

ALTER TABLE protocol_sessions
    ADD CONSTRAINT protocol_sessions_audit_subject_check CHECK (length(audit_subject) BETWEEN 1 AND 256),
    ADD CONSTRAINT protocol_sessions_quota_subject_check CHECK (length(quota_subject) BETWEEN 1 AND 1024);

ALTER TABLE build_requests
    ADD COLUMN queue_state text NOT NULL DEFAULT 'completed',
    ADD COLUMN queued_at timestamptz,
    ADD COLUMN audit_subject text NOT NULL DEFAULT 'gate-three',
    ADD COLUMN quota_subject text NOT NULL DEFAULT 'gate-three';

ALTER TABLE build_requests
    ADD CONSTRAINT build_requests_queue_state_check CHECK (
        queue_state IN ('accepted', 'queued', 'dispatching', 'backend-pending', 'running', 'collecting', 'completed', 'failed', 'cancelled')
    ),
    ADD CONSTRAINT build_requests_queued_at_check CHECK (
        (queue_state = 'accepted' AND queued_at IS NULL)
        OR (queue_state != 'accepted' AND (queue_state = 'completed' OR queued_at IS NOT NULL))
    ),
    ADD CONSTRAINT build_requests_audit_subject_check CHECK (length(audit_subject) BETWEEN 1 AND 256),
    ADD CONSTRAINT build_requests_quota_subject_check CHECK (length(quota_subject) BETWEEN 1 AND 1024);

CREATE TABLE execution_attempts (
    attempt_id text PRIMARY KEY CONSTRAINT execution_attempts_attempt_id_check CHECK (length(attempt_id) BETWEEN 1 AND 4096),
    request_id text NOT NULL REFERENCES build_requests(request_id),
    ordinal integer NOT NULL CONSTRAINT execution_attempts_ordinal_check CHECK (ordinal > 0),
    idempotency_key text NOT NULL UNIQUE CONSTRAINT execution_attempts_idempotency_key_check CHECK (length(idempotency_key) BETWEEN 1 AND 4096),
    backend text NOT NULL CONSTRAINT execution_attempts_backend_check CHECK (length(backend) BETWEEN 1 AND 4096),
    backend_execution_id text CONSTRAINT execution_attempts_backend_execution_id_check CHECK (backend_execution_id IS NULL OR length(backend_execution_id) BETWEEN 1 AND 4096),
    state text NOT NULL CONSTRAINT execution_attempts_state_check CHECK (
        state IN ('dispatching', 'backend-pending', 'running', 'collecting', 'succeeded', 'failed', 'cancelled')
    ),
    created_at timestamptz NOT NULL DEFAULT now(),
    submitted_at timestamptz,
    started_at timestamptz,
    collecting_at timestamptz,
    completed_at timestamptz,
    fenced_at timestamptz,
    UNIQUE (request_id, ordinal),
    CONSTRAINT execution_attempts_timestamp_order_check CHECK (
        (submitted_at IS NULL OR submitted_at >= created_at)
        AND (started_at IS NULL OR (submitted_at IS NOT NULL AND started_at >= submitted_at))
        AND (collecting_at IS NULL OR (started_at IS NOT NULL AND collecting_at >= started_at))
        AND (completed_at IS NULL OR completed_at >= COALESCE(collecting_at, started_at, submitted_at, created_at))
        AND (fenced_at IS NULL OR fenced_at >= created_at)
    ),
    CONSTRAINT execution_attempts_state_timestamp_check CHECK (
        (state = 'dispatching' AND submitted_at IS NULL AND started_at IS NULL AND collecting_at IS NULL AND completed_at IS NULL)
        OR (state = 'backend-pending' AND submitted_at IS NOT NULL AND started_at IS NULL AND collecting_at IS NULL AND completed_at IS NULL)
        OR (state = 'running' AND submitted_at IS NOT NULL AND started_at IS NOT NULL AND collecting_at IS NULL AND completed_at IS NULL)
        OR (state = 'collecting' AND submitted_at IS NOT NULL AND started_at IS NOT NULL AND collecting_at IS NOT NULL AND completed_at IS NULL)
        OR (state IN ('succeeded', 'failed', 'cancelled') AND completed_at IS NOT NULL)
    )
);

CREATE UNIQUE INDEX execution_attempts_one_active_per_request_idx
    ON execution_attempts (request_id)
    WHERE state IN ('dispatching', 'backend-pending', 'running', 'collecting') AND fenced_at IS NULL;

CREATE TABLE execution_outcomes (
    attempt_id text PRIMARY KEY REFERENCES execution_attempts(attempt_id),
    classification text NOT NULL CONSTRAINT execution_outcomes_classification_check CHECK (length(classification) BETWEEN 1 AND 4096),
    result_metadata jsonb NOT NULL DEFAULT '{}'::jsonb CONSTRAINT execution_outcomes_result_metadata_check CHECK (jsonb_typeof(result_metadata) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE capacity_reservations (
    reservation_id text PRIMARY KEY CONSTRAINT capacity_reservations_reservation_id_check CHECK (length(reservation_id) BETWEEN 1 AND 4096),
    attempt_id text NOT NULL REFERENCES execution_attempts(attempt_id),
    phase text NOT NULL CONSTRAINT capacity_reservations_phase_check CHECK (phase IN ('dispatching', 'backend-pending', 'running', 'collecting')),
    quota_subject text NOT NULL CONSTRAINT capacity_reservations_quota_subject_check CHECK (length(quota_subject) BETWEEN 1 AND 4096),
    units integer NOT NULL CONSTRAINT capacity_reservations_units_check CHECK (units > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    released_at timestamptz,
    CONSTRAINT capacity_reservations_released_at_check CHECK (released_at IS NULL OR released_at >= created_at)
);

CREATE UNIQUE INDEX capacity_reservations_one_active_phase_idx
    ON capacity_reservations (attempt_id, phase)
    WHERE released_at IS NULL;
