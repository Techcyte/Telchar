ALTER TABLE shared_build_attempts ADD COLUMN producer_location text
    CHECK (octet_length(producer_location) BETWEEN 1 AND 4096);

CREATE TABLE output_producers (
    store_path text PRIMARY KEY CHECK (octet_length(store_path) BETWEEN 1 AND 4096),
    attempt_id bigint NOT NULL REFERENCES shared_build_attempts(attempt_id),
    backend_name text NOT NULL CHECK (octet_length(backend_name) BETWEEN 1 AND 4096),
    backend_kind text NOT NULL CHECK (backend_kind IN ('local', 'static-ssh', 'nomad')),
    location text CHECK (octet_length(location) BETWEEN 1 AND 4096)
);

CREATE TRIGGER singleton_ownership_fence
    BEFORE INSERT OR UPDATE OR DELETE ON output_producers
    FOR EACH STATEMENT EXECUTE FUNCTION enforce_singleton_ownership();
