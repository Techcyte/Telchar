-- Extends lifecycle states and reconciliation metadata for restart recovery.
ALTER TABLE build_requests
    DROP CONSTRAINT build_requests_queue_state_check,
    ADD CONSTRAINT build_requests_queue_state_check CHECK (
        queue_state IN ('accepted', 'queued', 'dispatching', 'reconciling', 'backend-pending', 'running', 'collecting', 'completed', 'failed', 'cancelled')
    );

ALTER TABLE execution_attempts
    DROP CONSTRAINT execution_attempts_state_check,
    DROP CONSTRAINT execution_attempts_state_timestamp_check,
    ADD CONSTRAINT execution_attempts_state_check CHECK (
        state IN ('dispatching', 'reconciling', 'backend-pending', 'running', 'collecting', 'succeeded', 'failed', 'cancelled')
    ),
    ADD CONSTRAINT execution_attempts_state_timestamp_check CHECK (
        (state = 'dispatching' AND submitted_at IS NULL AND started_at IS NULL AND collecting_at IS NULL AND completed_at IS NULL)
        OR (state = 'reconciling' AND submitted_at IS NULL AND started_at IS NULL AND collecting_at IS NULL AND completed_at IS NULL)
        OR (state = 'backend-pending' AND submitted_at IS NOT NULL AND started_at IS NULL AND collecting_at IS NULL AND completed_at IS NULL)
        OR (state = 'running' AND submitted_at IS NOT NULL AND started_at IS NOT NULL AND collecting_at IS NULL AND completed_at IS NULL)
        OR (state = 'collecting' AND submitted_at IS NOT NULL AND started_at IS NOT NULL AND collecting_at IS NOT NULL AND completed_at IS NULL)
        OR (state IN ('succeeded', 'failed', 'cancelled') AND completed_at IS NOT NULL)
    ),
    ADD CONSTRAINT execution_attempts_reconciliation_fence_check CHECK (
        (state = 'reconciling' AND fenced_at IS NOT NULL)
        OR (state != 'reconciling' AND fenced_at IS NULL)
    );
