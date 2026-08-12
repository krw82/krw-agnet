BEGIN;

-- A small operational receipt for the local Rust daemon. This is not
-- ontology content and it never contains a user question, answer, evidence,
-- prompt, tool input, or provider credential. The GCP web process reads it
-- through its server-only Supabase role before creating a durable run.
CREATE TABLE IF NOT EXISTS public.agent_v1_daemon_heartbeats (
    daemon_id text PRIMARY KEY
        CHECK (char_length(daemon_id) BETWEEN 1 AND 128),
    provider text NOT NULL
        CHECK (provider IN ('glm', 'deepseek')),
    descriptor_artifact_hash text NOT NULL
        CHECK (descriptor_artifact_hash ~ '^sha256:[0-9a-f]{64}$'),
    release_set_hash text NOT NULL
        CHECK (release_set_hash ~ '^sha256:[0-9a-f]{64}$'),
    runtime_version text NOT NULL
        CHECK (char_length(runtime_version) BETWEEN 1 AND 128),
    last_seen_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    heartbeat_expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX IF NOT EXISTS agent_v1_daemon_heartbeats_ready_idx
    ON public.agent_v1_daemon_heartbeats (
        provider,
        descriptor_artifact_hash,
        release_set_hash,
        heartbeat_expires_at DESC,
        last_seen_at DESC
    );

ALTER TABLE public.agent_v1_daemon_heartbeats ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON TABLE public.agent_v1_daemon_heartbeats FROM PUBLIC;
DO $table_grants$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
        REVOKE ALL ON TABLE public.agent_v1_daemon_heartbeats FROM anon;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
        REVOKE ALL ON TABLE public.agent_v1_daemon_heartbeats FROM authenticated;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'service_role') THEN
        GRANT SELECT ON TABLE public.agent_v1_daemon_heartbeats TO service_role;
    END IF;
END;
$table_grants$;

CREATE OR REPLACE FUNCTION agent_v1.heartbeat_daemon(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store, public
AS $function$
DECLARE
    v_ttl_ms bigint;
    v_expires_at timestamptz;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','daemon_id','provider','descriptor_artifact_hash',
              'release_set_hash','runtime_version','heartbeat_ttl_ms'],
        ARRAY['abi_version','daemon_id','provider','descriptor_artifact_hash',
              'release_set_hash','runtime_version','heartbeat_ttl_ms']
    );
    PERFORM agent_store.assert_text(p_request, 'daemon_id', 128);
    PERFORM agent_store.assert_text(p_request, 'provider', 16);
    PERFORM agent_store.assert_hash(p_request->>'descriptor_artifact_hash');
    PERFORM agent_store.assert_hash(p_request->>'release_set_hash');
    PERFORM agent_store.assert_text(p_request, 'runtime_version', 128);
    IF p_request->>'provider' NOT IN ('glm', 'deepseek') THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'invalid_daemon_provider';
    END IF;
    v_ttl_ms := agent_store.request_bigint(p_request, 'heartbeat_ttl_ms', 30000, 120000);
    v_expires_at := clock_timestamp() + v_ttl_ms * interval '1 millisecond';

    INSERT INTO public.agent_v1_daemon_heartbeats(
        daemon_id, provider, descriptor_artifact_hash, release_set_hash,
        runtime_version, last_seen_at, heartbeat_expires_at, updated_at
    ) VALUES (
        p_request->>'daemon_id', p_request->>'provider',
        p_request->>'descriptor_artifact_hash', p_request->>'release_set_hash',
        p_request->>'runtime_version', clock_timestamp(), v_expires_at, clock_timestamp()
    ) ON CONFLICT (daemon_id) DO UPDATE SET
        provider = EXCLUDED.provider,
        descriptor_artifact_hash = EXCLUDED.descriptor_artifact_hash,
        release_set_hash = EXCLUDED.release_set_hash,
        runtime_version = EXCLUDED.runtime_version,
        last_seen_at = EXCLUDED.last_seen_at,
        heartbeat_expires_at = EXCLUDED.heartbeat_expires_at,
        updated_at = EXCLUDED.updated_at;

    RETURN jsonb_build_object(
        'ready', true,
        'heartbeat_expires_at', v_expires_at
    );
END;
$function$;

REVOKE ALL ON FUNCTION agent_v1.heartbeat_daemon(jsonb) FROM PUBLIC;
DO $grant$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'krw_agent_daemon') THEN
        GRANT EXECUTE ON FUNCTION agent_v1.heartbeat_daemon(jsonb) TO krw_agent_daemon;
    END IF;
END;
$grant$;

COMMIT;
