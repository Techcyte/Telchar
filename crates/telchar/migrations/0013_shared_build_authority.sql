DROP TABLE capacity_reservations;
DROP TABLE execution_outcomes;
DROP TABLE execution_attempts;

ALTER TABLE build_requests
    DROP CONSTRAINT build_requests_queue_state_check,
    DROP CONSTRAINT build_requests_queued_at_check,
    DROP COLUMN queue_state,
    DROP COLUMN queued_at;
