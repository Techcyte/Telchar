ALTER TABLE shared_builds
    DROP CONSTRAINT shared_builds_backend_kind_check,
    ADD CONSTRAINT shared_builds_backend_kind_check CHECK (
        backend_kind IN ('local', 'static-ssh', 'nomad', 'microvm')
    );

ALTER TABLE shared_build_attempts
    DROP CONSTRAINT shared_build_attempts_backend_kind_check,
    ADD CONSTRAINT shared_build_attempts_backend_kind_check CHECK (
        backend_kind IN ('local', 'static-ssh', 'nomad', 'microvm')
    );
