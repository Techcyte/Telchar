ALTER TABLE store_leases ADD COLUMN expires_at timestamptz;

UPDATE store_leases
SET expires_at = transaction_timestamp() + interval '1 hour'
WHERE purpose = 'output' AND state = 'active';

UPDATE store_leases
SET expires_at = transaction_timestamp()
WHERE purpose = 'output' AND state = 'released';

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM store_leases
        WHERE purpose = 'output' AND state = 'active' AND created_at > transaction_timestamp()
    ) THEN
        RAISE EXCEPTION 'store_leases contains active output leases created after the migration transaction timestamp';
    END IF;
END
$$;

ALTER TABLE store_leases
    ADD CONSTRAINT store_leases_expires_at_purpose_check CHECK (
        (purpose = 'output' AND expires_at IS NOT NULL)
        OR (purpose != 'output' AND expires_at IS NULL)
    );

ALTER TABLE store_leases
    ADD CONSTRAINT store_leases_expires_at_active_output_check CHECK (
        NOT (purpose = 'output' AND state = 'active') OR expires_at >= created_at
    );

CREATE INDEX store_leases_active_output_expiry_idx ON store_leases (expires_at, lease_id)
    WHERE owner_kind = 'request' AND purpose = 'output' AND state = 'active';
