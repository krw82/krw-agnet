-- Restore the single authoritative Agent-owned final projection contract.
-- The frontend product migration may consume this function, but must not
-- redefine it. Invalid optional visualizations are projected as an empty list;
-- the committed Markdown answer remains authoritative and readable.

BEGIN;

CREATE OR REPLACE FUNCTION agent_v1.read_final_projection(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_bundle agent_store.answer_bundles%ROWTYPE;
    v_markdown text;
    v_visualizations jsonb;
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
    IF v_run.state <> 'final' THEN
        RAISE EXCEPTION USING ERRCODE='K1024',MESSAGE='final_output_unavailable';
    END IF;

    SELECT * INTO v_bundle FROM agent_store.answer_bundles WHERE run_id=v_run.run_id;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='final_bundle_missing'; END IF;
    IF jsonb_typeof(v_bundle.answer_bundle->'output') NOT IN ('string','object')
       OR jsonb_typeof(v_bundle.answer_bundle->'rendered_markdown') <> 'string' THEN
        RAISE EXCEPTION USING ERRCODE='K1026',MESSAGE='final_bundle_shape_invalid';
    END IF;
    v_markdown := v_bundle.answer_bundle->>'rendered_markdown';
    IF v_markdown = '' THEN RAISE EXCEPTION USING ERRCODE='K1026',MESSAGE='final_markdown_empty'; END IF;

    v_visualizations := COALESCE(v_bundle.answer_bundle->'visualizations','[]'::jsonb);
    IF jsonb_typeof(v_visualizations) <> 'array' THEN
        v_visualizations := '[]'::jsonb;
    ELSIF jsonb_array_length(v_visualizations) > 3
       OR EXISTS (SELECT 1 FROM jsonb_array_elements(v_visualizations) element
                  WHERE jsonb_typeof(element) <> 'object'
                     OR element->>'artifact_format' <> 'krw-visualization'
                     OR element->>'schema_version' <> '4'
                     OR jsonb_typeof(element->'provenance') <> 'object'
                     OR element ?| array['html', 'markup', 'svg', 'jsx', 'renderer_code']
                     OR CASE
                          WHEN jsonb_typeof(element->'views') <> 'array' THEN true
                          ELSE jsonb_array_length(element->'views') NOT BETWEEN 1 AND 4
                        END) THEN
        v_visualizations := '[]'::jsonb;
    END IF;

    RETURN jsonb_build_object(
        'run_id',v_run.run_id,
        'answer_bundle_hash',v_bundle.answer_bundle_hash,
        'final_output_hash',v_bundle.final_output_hash,
        'markdown',v_markdown,
        'visualizations',v_visualizations,
        'usage',v_bundle.usage,
        'evidence_ledger_hash',v_bundle.answer_bundle->>'evidence_ledger_hash',
        'memory_revision',v_run.terminal_outcome->>'memory_revision',
        'memory_frontier_hash',v_bundle.answer_bundle->>'memory_frontier_hash'
    );
END;
$$;

COMMIT;
