CREATE TABLE microvm_resolutions (
    resolution_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    intent_id text NOT NULL REFERENCES microvm_launches(intent_id),
    action text NOT NULL CHECK (action = 'risk-release'),
    provider_id text CHECK (length(provider_id) BETWEEN 1 AND 256),
    actor_uid bigint NOT NULL CHECK (actor_uid = 0),
    owner_generation bigint NOT NULL CHECK (owner_generation > 0),
    reason text NOT NULL CHECK (octet_length(reason) BETWEEN 1 AND 1024),
    evidence_reference text NOT NULL CHECK (octet_length(evidence_reference) BETWEEN 1 AND 1024),
    recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (intent_id, action)
);

CREATE TRIGGER singleton_ownership_fence
    BEFORE INSERT OR UPDATE OR DELETE ON microvm_resolutions
    FOR EACH STATEMENT EXECUTE FUNCTION enforce_singleton_ownership();

CREATE FUNCTION preserve_microvm_resolution() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'MicroVM resolution audit is immutable' USING ERRCODE = '55000';
END
$$;

CREATE TRIGGER microvm_resolution_immutable
    BEFORE UPDATE OR DELETE OR TRUNCATE ON microvm_resolutions
    FOR EACH STATEMENT EXECUTE FUNCTION preserve_microvm_resolution();
