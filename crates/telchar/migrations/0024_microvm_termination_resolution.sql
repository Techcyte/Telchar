ALTER TABLE microvm_resolutions
    DROP CONSTRAINT microvm_resolutions_action_check,
    ADD CONSTRAINT microvm_resolutions_action_check
        CHECK (action IN ('risk-release', 'attach-identity', 'verify-termination')),
    ADD CONSTRAINT microvm_resolution_termination_identity
        CHECK (action <> 'verify-termination' OR provider_id IS NOT NULL);
