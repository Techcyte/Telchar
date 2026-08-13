-- Adds retained NAR-size accounting to store leases.
ALTER TABLE store_leases
    ADD COLUMN nar_size bigint
    CONSTRAINT store_leases_nar_size_check CHECK (nar_size IS NULL OR nar_size > 0);

UPDATE store_leases
SET nar_size = 1
WHERE purpose IN ('derivation', 'input');

ALTER TABLE store_leases
    ADD CONSTRAINT store_leases_retained_size_check CHECK (
        (purpose IN ('derivation', 'input') AND nar_size IS NOT NULL)
        OR (purpose NOT IN ('derivation', 'input') AND nar_size IS NULL)
    );
