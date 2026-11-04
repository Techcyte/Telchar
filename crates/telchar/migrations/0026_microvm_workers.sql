CREATE TABLE microvm_workers (
    intent_id text PRIMARY KEY REFERENCES microvm_launches(intent_id),
    state text NOT NULL CHECK (state IN ('busy', 'idle', 'draining')),
    assignment_epoch bigint NOT NULL DEFAULT 1 CHECK (assignment_epoch > 0),
    attempt_id bigint UNIQUE REFERENCES shared_build_attempts(attempt_id),
    expires_at timestamptz NOT NULL,
    idle_since timestamptz,
    completed_jobs bigint NOT NULL DEFAULT 0 CHECK (completed_jobs >= 0),
    CHECK ((state = 'busy') = (attempt_id IS NOT NULL)),
    CHECK ((state = 'idle') = (idle_since IS NOT NULL))
);

CREATE TRIGGER singleton_ownership_fence
    BEFORE INSERT OR UPDATE OR DELETE ON microvm_workers
    FOR EACH STATEMENT EXECUTE FUNCTION enforce_singleton_ownership();

CREATE FUNCTION track_microvm_worker() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.phase = 'bound' AND OLD.provider_id IS NULL THEN
        INSERT INTO microvm_workers (intent_id, state, attempt_id, expires_at)
        VALUES (NEW.intent_id, 'busy', NEW.attempt_id,
                NEW.created_at + make_interval(secs => NEW.maximum_duration_seconds));
    ELSIF NEW.phase = 'closed' THEN
        UPDATE microvm_workers SET state = 'draining', attempt_id = NULL, idle_since = NULL
        WHERE intent_id = NEW.intent_id;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER microvm_worker_lifecycle AFTER UPDATE ON microvm_launches
    FOR EACH ROW EXECUTE FUNCTION track_microvm_worker();
