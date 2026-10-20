ALTER TABLE microvm_launches
    ADD COLUMN termination_observed_at timestamptz;

UPDATE microvm_launches
SET termination_observed_at = reservation_released_at
WHERE provider_id IS NOT NULL AND reservation_released_at IS NOT NULL;

ALTER TABLE microvm_launches
    ADD CONSTRAINT microvm_termination_identity
    CHECK (termination_observed_at IS NULL OR
           (provider_id IS NOT NULL AND reservation_released_at IS NOT NULL AND phase = 'closed'));

CREATE INDEX microvm_launches_cleanup_identity_idx
    ON microvm_launches (account_id, region, intent_id)
    WHERE provider_id IS NOT NULL AND termination_observed_at IS NULL;
