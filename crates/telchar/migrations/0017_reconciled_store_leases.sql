-- Records successful GC-root reconciliation without discarding lease history.
ALTER TABLE store_leases
    DROP CONSTRAINT store_leases_state_check;

ALTER TABLE store_leases
    ADD CONSTRAINT store_leases_state_check
    CHECK (state IN ('active', 'released', 'reconciled'));

ALTER TABLE store_leases
    DROP CONSTRAINT store_leases_released_at_check;

ALTER TABLE store_leases
    ADD CONSTRAINT store_leases_released_at_check CHECK (
        (state = 'active' AND released_at IS NULL)
        OR (state IN ('released', 'reconciled') AND released_at IS NOT NULL AND released_at >= created_at)
    );

ALTER TABLE store_leases
    ADD COLUMN reconciled_at timestamptz;

ALTER TABLE store_leases
    ADD CONSTRAINT store_leases_reconciled_at_check CHECK (
        (state IN ('active', 'released') AND reconciled_at IS NULL)
        OR (
            state = 'reconciled'
            AND released_at IS NOT NULL
            AND reconciled_at IS NOT NULL
            AND reconciled_at >= released_at
        )
    );
