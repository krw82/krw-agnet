-- Host-only terminal action trace for operator quality review.
--
-- The projection deliberately exposes only a bounded action sequence:
-- capability ID, terminal action stage, and opaque result hash. It never
-- returns provider episodes, prompts, arguments, evidence bodies, artifact
-- references, or raw MCP results. Exact tenant + principal + session + run
-- ownership is required before the trace can be read.

BEGIN;

CREATE FUNCTION agent_v1.read_terminal_trace(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','run_id','tenant_id','principal_id','session_id'],
        ARRAY['abi_version','run_id','tenant_id','principal_id','session_id']
    );
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'principal_id',128);
    PERFORM agent_store.assert_text(p_request,'session_id',128);

    SELECT * INTO v_run FROM agent_store.runs WHERE run_id=p_request->>'run_id';
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch';
    END IF;
    IF v_run.principal_id IS DISTINCT FROM p_request->>'principal_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1027',MESSAGE='principal_mismatch';
    END IF;
    IF v_run.session_id IS DISTINCT FROM p_request->>'session_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1028',MESSAGE='session_mismatch';
    END IF;
    IF v_run.state NOT IN ('final','cancelled','failed') THEN
        RAISE EXCEPTION USING ERRCODE='K1024',MESSAGE='terminal_trace_unavailable';
    END IF;

    RETURN jsonb_build_object(
        'run_id',v_run.run_id,
        'state',v_run.state,
        'actions',COALESCE((
            SELECT jsonb_agg(jsonb_build_object(
                'capability_id',action.capability_id,
                'stage',action.stage,
                'result_hash',action.result_hash
            ) ORDER BY action.begun_at, action.action_key)
              FROM agent_store.actions AS action
             WHERE action.run_id=v_run.run_id
        ), '[]'::jsonb)
    );
END;
$$;

REVOKE ALL ON FUNCTION agent_v1.read_terminal_trace(jsonb) FROM PUBLIC;

COMMENT ON FUNCTION agent_v1.read_terminal_trace(jsonb) IS
    'Exact-owner terminal action trace for operator review; excludes prompts, arguments, raw results, artifacts, and evidence bodies.';

COMMIT;
