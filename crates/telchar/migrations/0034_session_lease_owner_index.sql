CREATE INDEX store_leases_session_owner_idx
    ON store_leases (owner_id, lease_id)
    WHERE owner_kind = 'session';
