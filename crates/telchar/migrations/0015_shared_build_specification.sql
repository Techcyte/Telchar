-- Persists the exact admitted build specification required by detached Nomad callbacks.
ALTER TABLE shared_builds
    ADD COLUMN build_request jsonb CONSTRAINT shared_builds_build_request_check CHECK (
        build_request IS NULL
        OR (
            jsonb_typeof(build_request) = 'object'
            AND octet_length(build_request::text) <= 1048576
        )
    );
