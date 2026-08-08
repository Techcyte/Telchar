CREATE TABLE protocol_sessions (
    session_id text PRIMARY KEY,
    requester_reference text NOT NULL,
    state text NOT NULL CONSTRAINT protocol_sessions_state_check CHECK (state IN ('open', 'closed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    closed_at timestamptz,
    CONSTRAINT protocol_sessions_closed_at_check CHECK (
        (state = 'open' AND closed_at IS NULL)
        OR (state = 'closed' AND closed_at IS NOT NULL AND closed_at >= created_at)
    )
);

CREATE TABLE build_requests (
    request_id text PRIMARY KEY CONSTRAINT build_requests_request_id_check CHECK (length(request_id) > 0),
    derivation_path text NOT NULL CONSTRAINT build_requests_derivation_path_check CHECK (length(derivation_path) > 0),
    system text NOT NULL CONSTRAINT build_requests_system_check CHECK (length(system) > 0),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE request_attachments (
    session_id text NOT NULL REFERENCES protocol_sessions(session_id),
    request_id text NOT NULL REFERENCES build_requests(request_id),
    state text NOT NULL CONSTRAINT request_attachments_state_check CHECK (state IN ('attached', 'detached')),
    attached_at timestamptz NOT NULL DEFAULT now(),
    detached_at timestamptz,
    PRIMARY KEY (session_id, request_id),
    CONSTRAINT request_attachments_detached_at_check CHECK (
        (state = 'attached' AND detached_at IS NULL)
        OR (state = 'detached' AND detached_at IS NOT NULL AND detached_at >= attached_at)
    )
);

CREATE TABLE store_leases (
    lease_id text PRIMARY KEY CONSTRAINT store_leases_lease_id_check CHECK (length(lease_id) > 0),
    owner_kind text NOT NULL CONSTRAINT store_leases_owner_kind_check CHECK (owner_kind IN ('session', 'request')),
    owner_id text NOT NULL CONSTRAINT store_leases_owner_id_check CHECK (length(owner_id) > 0),
    store_path text NOT NULL CONSTRAINT store_leases_store_path_check CHECK (length(store_path) > 0),
    purpose text NOT NULL CONSTRAINT store_leases_purpose_check CHECK (length(purpose) > 0),
    state text NOT NULL CONSTRAINT store_leases_state_check CHECK (state IN ('active', 'released')),
    created_at timestamptz NOT NULL DEFAULT now(),
    released_at timestamptz,
    CONSTRAINT store_leases_released_at_check CHECK (
        (state = 'active' AND released_at IS NULL)
        OR (state = 'released' AND released_at IS NOT NULL AND released_at >= created_at)
    )
);
