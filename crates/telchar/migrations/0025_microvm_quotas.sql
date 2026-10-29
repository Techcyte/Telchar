CREATE TABLE microvm_quota_reservations (
    intent_id text PRIMARY KEY REFERENCES microvm_launches(intent_id),
    unit text NOT NULL CHECK (unit IN ('vm-seconds', 'usd-picodollars')),
    allowance bigint NOT NULL CHECK (allowance > 0),
    amount bigint NOT NULL CHECK (amount > 0 AND amount <= allowance),
    pricing_reference text CHECK (length(pricing_reference) BETWEEN 1 AND 2048),
    pricing_expires_at timestamptz,
    dispatched_at timestamptz,
    CHECK ((unit = 'vm-seconds' AND pricing_reference IS NULL AND pricing_expires_at IS NULL)
        OR (unit = 'usd-picodollars' AND pricing_reference IS NOT NULL AND pricing_expires_at IS NOT NULL))
);

CREATE TRIGGER singleton_ownership_fence
    BEFORE INSERT OR UPDATE OR DELETE ON microvm_quota_reservations
    FOR EACH STATEMENT EXECUTE FUNCTION enforce_singleton_ownership();

CREATE INDEX microvm_launches_account_region_created_idx
    ON microvm_launches (account_id, region, created_at);
