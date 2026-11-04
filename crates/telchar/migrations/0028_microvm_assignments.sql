CREATE TABLE microvm_assignments (
    intent_id text NOT NULL REFERENCES microvm_workers(intent_id),
    assignment_epoch bigint NOT NULL CHECK (assignment_epoch > 0),
    attempt_id bigint NOT NULL UNIQUE REFERENCES shared_build_attempts(attempt_id),
    credential_verifier bytea CHECK (octet_length(credential_verifier) = 32),
    admitted_at timestamptz,
    completed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (intent_id, assignment_epoch)
);

INSERT INTO microvm_assignments (intent_id, assignment_epoch, attempt_id)
SELECT intent_id, assignment_epoch, attempt_id FROM microvm_workers WHERE state = 'busy';

CREATE TRIGGER singleton_ownership_fence
    BEFORE INSERT OR UPDATE OR DELETE ON microvm_assignments
    FOR EACH STATEMENT EXECUTE FUNCTION enforce_singleton_ownership();

CREATE FUNCTION track_microvm_assignment() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' OR NEW.assignment_epoch <> OLD.assignment_epoch THEN
        INSERT INTO microvm_assignments (intent_id, assignment_epoch, attempt_id)
        VALUES (NEW.intent_id, NEW.assignment_epoch, NEW.attempt_id);
    ELSIF OLD.state = 'busy' AND NEW.state <> 'busy' THEN
        UPDATE microvm_assignments SET completed_at = clock_timestamp()
        WHERE intent_id = NEW.intent_id AND assignment_epoch = NEW.assignment_epoch;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER microvm_assignment_lifecycle AFTER INSERT OR UPDATE ON microvm_workers
    FOR EACH ROW EXECUTE FUNCTION track_microvm_assignment();
