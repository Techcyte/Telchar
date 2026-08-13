-- Adds trusted quota ownership and durable queue ordering to shared builds.
ALTER TABLE shared_builds
    ADD COLUMN quota_subject text,
    ADD COLUMN queue_position bigint,
    ADD COLUMN queued_at timestamptz;

ALTER TABLE shared_builds
    ADD CONSTRAINT shared_builds_quota_subject_check CHECK (
        quota_subject IS NULL OR length(quota_subject) BETWEEN 1 AND 4096
    ),
    ADD CONSTRAINT shared_builds_queue_position_check CHECK (
        queue_position IS NULL OR queue_position > 0
    ),
    ADD CONSTRAINT shared_builds_queue_metadata_check CHECK (
        (quota_subject IS NULL AND queue_position IS NULL AND queued_at IS NULL)
        OR (quota_subject IS NOT NULL AND queue_position IS NOT NULL AND queued_at IS NOT NULL)
    );

CREATE SEQUENCE shared_build_queue_position_seq;

CREATE INDEX shared_builds_subject_queue_idx
    ON shared_builds (quota_subject, queue_position)
    WHERE state = 'claimed' AND quota_subject IS NOT NULL;

CREATE INDEX shared_builds_subject_active_idx
    ON shared_builds (quota_subject, started_at)
    WHERE state IN ('running', 'collecting') AND quota_subject IS NOT NULL;
