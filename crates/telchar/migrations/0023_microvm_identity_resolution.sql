ALTER TABLE microvm_resolutions
    DROP CONSTRAINT microvm_resolutions_action_check,
    ADD CONSTRAINT microvm_resolutions_action_check
        CHECK (action IN ('risk-release', 'attach-identity')),
    ADD CONSTRAINT microvm_resolution_attachment_identity
        CHECK (action <> 'attach-identity' OR provider_id IS NOT NULL);
