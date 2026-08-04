-- Host-only final presentation read. This intentionally exposes neither
-- provider episodes, MCP results, evidence bodies, nor recovery artifacts.

BEGIN;

CREATE FUNCTION agent_v1.read_final_output(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_bundle agent_store.answer_bundles%ROWTYPE;
    v_markdown text;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','run_id','tenant_id'],
        ARRAY['abi_version','run_id','tenant_id']
    );
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);

    SELECT * INTO v_run FROM agent_store.runs WHERE run_id=p_request->>'run_id';
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch';
    END IF;
    IF v_run.state <> 'final' THEN
        RAISE EXCEPTION USING ERRCODE='K1024',MESSAGE='final_output_unavailable';
    END IF;

    SELECT * INTO v_bundle FROM agent_store.answer_bundles WHERE run_id=v_run.run_id;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='final_bundle_missing'; END IF;
    IF jsonb_typeof(v_bundle.answer_bundle->'output') <> 'string'
       OR jsonb_typeof(v_bundle.answer_bundle->'rendered_markdown') <> 'string' THEN
        RAISE EXCEPTION USING ERRCODE='K1026',MESSAGE='final_bundle_shape_invalid';
    END IF;
    v_markdown := v_bundle.answer_bundle->>'rendered_markdown';
    IF v_markdown = '' THEN RAISE EXCEPTION USING ERRCODE='K1026',MESSAGE='final_markdown_empty'; END IF;

    RETURN jsonb_build_object(
        'run_id',v_run.run_id,
        'final_output_hash',v_bundle.final_output_hash,
        'markdown',v_markdown
    );
END;
$$;

COMMIT;
