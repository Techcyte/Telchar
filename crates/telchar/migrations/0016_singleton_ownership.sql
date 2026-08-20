CREATE TABLE singleton_ownership (
    owner_kind text PRIMARY KEY CHECK (owner_kind IN ('daemon', 'local-executor')),
    owner_token text NOT NULL CHECK (owner_token <> ''),
    generation bigint NOT NULL CHECK (generation > 0),
    lease_expires_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
