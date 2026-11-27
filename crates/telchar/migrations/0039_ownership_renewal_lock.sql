-- Ownership identity must be a PostgreSQL key: KEY SHARE then conflicts with
-- acquisition/takeover identity updates, but not timestamp-only lease renewal.
-- This index and the trigger lock form one fencing contract; do not remove one
-- independently. Existing owner_kind primary key guarantees uniqueness here.
CREATE UNIQUE INDEX singleton_ownership_identity
    ON singleton_ownership(owner_kind, owner_token, generation);

CREATE OR REPLACE FUNCTION enforce_singleton_ownership() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    configured_owner_kind text := current_setting('telchar.owner_kind', true);
    configured_owner_token text := current_setting('telchar.owner_token', true);
    configured_generation bigint;
BEGIN
    IF configured_owner_kind IS NULL OR configured_owner_kind = '' THEN
        RETURN NEW;
    END IF;
    configured_generation := current_setting('telchar.owner_generation')::bigint;
    PERFORM 1
    FROM singleton_ownership
    WHERE owner_kind = configured_owner_kind
      AND owner_token = configured_owner_token
      AND generation = configured_generation
      AND lease_expires_at > clock_timestamp()
    FOR KEY SHARE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'singleton ownership fenced' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END
$$;
