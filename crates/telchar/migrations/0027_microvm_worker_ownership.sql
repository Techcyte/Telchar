ALTER TABLE microvm_workers ADD COLUMN owner_generation bigint NOT NULL
    DEFAULT COALESCE(NULLIF(current_setting('telchar.owner_generation', true), '')::bigint, 0)
    CHECK (owner_generation >= 0);
