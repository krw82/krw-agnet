-- Keep daemon admission tied to the same MCP routes that the Rust run engine
-- will call. A database heartbeat without this bit is only process liveness.
BEGIN;

ALTER TABLE public.agent_v1_daemon_heartbeats
    ADD COLUMN IF NOT EXISTS mcp_ready boolean NOT NULL DEFAULT false;

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
              'release_set_hash','runtime_version','heartbeat_ttl_ms','mcp_ready'],
        ARRAY['abi_version','daemon_id','provider','descriptor_artifact_hash',
              'release_set_hash','runtime_version','heartbeat_ttl_ms','mcp_ready']
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
        runtime_version, mcp_ready, last_seen_at, heartbeat_expires_at, updated_at
    ) VALUES (
        p_request->>'daemon_id', p_request->>'provider',
        p_request->>'descriptor_artifact_hash', p_request->>'release_set_hash',
        p_request->>'runtime_version', agent_store.request_boolean(p_request, 'mcp_ready'),
        clock_timestamp(), v_expires_at, clock_timestamp()
    ) ON CONFLICT (daemon_id) DO UPDATE SET
        provider = EXCLUDED.provider,
        descriptor_artifact_hash = EXCLUDED.descriptor_artifact_hash,
        release_set_hash = EXCLUDED.release_set_hash,
        runtime_version = EXCLUDED.runtime_version,
        mcp_ready = EXCLUDED.mcp_ready,
        last_seen_at = EXCLUDED.last_seen_at,
        heartbeat_expires_at = EXCLUDED.heartbeat_expires_at,
        updated_at = EXCLUDED.updated_at;

    RETURN jsonb_build_object(
        'ready', true,
        'mcp_ready', agent_store.request_boolean(p_request, 'mcp_ready'),
        'heartbeat_expires_at', v_expires_at
    );
END;
$function$;

COMMIT;
