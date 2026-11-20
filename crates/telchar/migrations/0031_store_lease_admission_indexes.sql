CREATE INDEX store_leases_active_retained_paths_idx ON store_leases (store_path) INCLUDE (nar_size)
    WHERE state = 'active' AND purpose IN ('derivation', 'input');

CREATE INDEX store_leases_request_owner_idx ON store_leases (owner_id, lease_id)
    WHERE owner_kind = 'request';
