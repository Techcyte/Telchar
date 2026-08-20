CREATE TABLE singleton_ownership (
    owner_kind text PRIMARY KEY CHECK (owner_kind IN ('daemon', 'local-executor')),
    owner_token text NOT NULL CHECK (owner_token <> ''),
    generation bigint NOT NULL CHECK (generation > 0),
    lease_expires_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE FUNCTION enforce_singleton_ownership() RETURNS trigger
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
    FOR SHARE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'singleton ownership fenced' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END
$$;

DO $$
DECLARE
    table_name text;
BEGIN
    FOREACH table_name IN ARRAY ARRAY[
        'protocol_sessions',
        'build_requests',
        'request_attachments',
        'store_leases',
        'local_backend_executions',
        'local_backend_execution_results',
        'shared_builds',
        'shared_build_scheduler_state',
        'shared_build_attempts',
        'shared_build_attempt_outcomes',
        'nomad_callback_nonces'
    ]
    LOOP
        EXECUTE format(
            'CREATE TRIGGER singleton_ownership_fence BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION enforce_singleton_ownership()',
            table_name
        );
    END LOOP;
END
$$;
