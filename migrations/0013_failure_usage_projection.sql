-- Make bounded token usage visible on failed terminal runs.
--
-- Failure settlement payloads already carry release-safe counters.  Project
-- only the `usage` object into read_committed_outcome so the Gateway can show
-- credit information without exposing the rest of the settlement payload.

BEGIN;

CREATE OR REPLACE FUNCTION agent_v1.read_committed_outcome(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_release jsonb;
    v_terminal_outcome jsonb;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','run_id','tenant_id'],
        ARRAY['abi_version','run_id','tenant_id']
    );
    SELECT * INTO v_run FROM agent_store.runs WHERE run_id=p_request->>'run_id';
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch';
    END IF;

    v_terminal_outcome := v_run.terminal_outcome;
    IF v_run.state='failed' THEN
        SELECT settlement_payload INTO v_release
          FROM agent_store.settlements
         WHERE run_id=v_run.run_id AND settlement_kind='released';
        IF jsonb_typeof(v_release->'usage')='object' THEN
            v_terminal_outcome := v_terminal_outcome || jsonb_build_object(
                'usage', v_release->'usage'
            );
        END IF;
    END IF;

    RETURN jsonb_build_object(
        'run_id',v_run.run_id,'state',v_run.state,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,
        'cancel_generation',v_run.cancel_generation,'terminal_outcome',v_terminal_outcome
    );
END;
$$;

REVOKE ALL ON FUNCTION agent_v1.read_committed_outcome(jsonb) FROM PUBLIC;

COMMENT ON FUNCTION agent_v1.read_committed_outcome(jsonb) IS
    'Read run state and bounded terminal outcome; failed runs may include release-safe usage counters only.';

COMMIT;
