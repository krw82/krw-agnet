BEGIN;

-- 2026-09-08 (deferral-resume re-proposals): the begin_action identity check
-- bound an action row to its original episode forever. A retryable read
-- whose attempt landed in the ambiguous lane and was later RE-PROPOSED from
-- a new episode (the natural recovery after a deferral resume) arrived with
-- a new episode_hash/tool_call_id and died as K1005 action_identity_conflict
-- — a row the engine may never re-execute. A retryable read is idempotent
-- by contract (canonical args), so re-binding the ambiguous row to the new
-- attempt is safe: reset stage to 'begun' under the new episode and let the
-- attempt execute. Every other mismatch (accepted/observed rows,
-- non-retryable reads, differing request hashes) keeps failing closed.
-- This is the 0001 function verbatim with the rebind block inserted ahead
-- of the episode-identity rejection in the existing-row branch.

CREATE OR REPLACE FUNCTION agent_v1.begin_action(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_action agent_store.actions%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','action_key','request_hash','episode_hash','tool_call_id',
              'capability_id','input_schema_hash','output_schema_hash','data_release_hash',
              'arguments_artifact_ref','retryable_read'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','action_key','request_hash','episode_hash','tool_call_id',
              'capability_id','input_schema_hash','output_schema_hash','data_release_hash',
              'arguments_artifact_ref','retryable_read']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'request_hash');
    PERFORM agent_store.assert_hash(p_request->>'episode_hash');
    PERFORM agent_store.assert_hash(p_request->>'input_schema_hash');
    PERFORM agent_store.assert_hash(p_request->>'output_schema_hash');
    PERFORM agent_store.assert_hash(p_request->>'data_release_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'action_key',128);
    PERFORM agent_store.assert_text(p_request,'tool_call_id',128);
    PERFORM agent_store.assert_text(p_request,'capability_id',128);
    PERFORM agent_store.assert_text(p_request,'arguments_artifact_ref',2048);
    SELECT * INTO v_run FROM agent_store.runs
     WHERE run_id = p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = 'K1001', MESSAGE = 'unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN RAISE EXCEPTION USING ERRCODE = 'K1017', MESSAGE = 'tenant_mismatch'; END IF;
    IF v_run.fencing_token <> agent_store.request_bigint(p_request,'fencing_token',0,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE = 'K1002', MESSAGE = 'stale_fence'; END IF;
    v_replay := agent_store.replay_mutation(v_run.run_id,p_request->>'mutation_id','begin_action',p_request->>'mutation_hash',p_request);
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state <> 'active' THEN RAISE EXCEPTION USING ERRCODE = 'K1003', MESSAGE = 'terminal_or_inactive_run'; END IF;
    IF v_run.lease_deadline <= clock_timestamp() THEN RAISE EXCEPTION USING ERRCODE = 'K1013', MESSAGE = 'lease_lost'; END IF;
    IF v_run.run_version <> agent_store.request_bigint(p_request,'expected_run_version',1,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE = 'K1012', MESSAGE = 'run_version_mismatch'; END IF;
    IF NOT EXISTS (
        SELECT 1 FROM agent_store.provider_episodes
         WHERE run_id = v_run.run_id AND episode_hash = p_request->>'episode_hash'
    ) THEN
        RAISE EXCEPTION USING ERRCODE = 'K1014', MESSAGE = 'episode_not_committed';
    END IF;

    SELECT * INTO v_action FROM agent_store.actions
     WHERE run_id = v_run.run_id AND action_key = p_request->>'action_key';
    IF FOUND THEN
        IF v_action.request_hash IS DISTINCT FROM p_request->>'request_hash'
           OR v_action.capability_id IS DISTINCT FROM p_request->>'capability_id'
           OR v_action.input_schema_hash IS DISTINCT FROM p_request->>'input_schema_hash'
           OR v_action.output_schema_hash IS DISTINCT FROM p_request->>'output_schema_hash'
           OR v_action.data_release_hash IS DISTINCT FROM p_request->>'data_release_hash'
           OR v_action.arguments_artifact_ref IS DISTINCT FROM p_request->>'arguments_artifact_ref'
           OR v_action.retryable_read IS DISTINCT FROM agent_store.request_boolean(p_request,'retryable_read') THEN
            RAISE EXCEPTION USING ERRCODE = 'K1005', MESSAGE = 'action_identity_conflict';
        END IF;
        IF v_action.stage = 'ambiguous'
           AND agent_store.request_boolean(p_request,'retryable_read')
           AND (v_action.episode_hash IS DISTINCT FROM p_request->>'episode_hash'
                OR v_action.tool_call_id IS DISTINCT FROM p_request->>'tool_call_id') THEN
            UPDATE agent_store.actions
               SET episode_hash = p_request->>'episode_hash',
                   tool_call_id = p_request->>'tool_call_id',
                   stage = 'begun',
                   result_hash = NULL
             WHERE run_id = v_run.run_id AND action_key = v_action.action_key;
            v_action.stage := 'begun';
            v_action.episode_hash := p_request->>'episode_hash';
            v_action.tool_call_id := p_request->>'tool_call_id';
            v_action.result_hash := NULL;
        END IF;
        IF v_action.episode_hash IS DISTINCT FROM p_request->>'episode_hash'
           OR v_action.tool_call_id IS DISTINCT FROM p_request->>'tool_call_id' THEN
            RAISE EXCEPTION USING ERRCODE = 'K1005', MESSAGE = 'action_identity_conflict';
        END IF;
        v_response := jsonb_build_object(
            'outcome','already_begun','run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
            'run_version',v_run.run_version,'action_key',v_action.action_key,'stage',v_action.stage,
            'request_hash',v_action.request_hash,'result_hash',v_action.result_hash,
            'retryable_read',v_action.retryable_read,
            'action_frontier_seq',v_run.action_frontier_seq,
            'action_frontier_hash',v_run.action_frontier_hash
        );
        PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','begin_action',p_request->>'mutation_hash',p_request,v_response);
        RETURN v_response;
    END IF;
    IF (
        SELECT count(*) FROM agent_store.actions
         WHERE run_id=v_run.run_id AND stage IN ('begun','observed','ambiguous')
    ) >= 128 THEN
        RAISE EXCEPTION USING ERRCODE='K1018',MESSAGE='recovery_receipt_limit_exceeded';
    END IF;
    IF (SELECT count(*) FROM agent_store.actions WHERE run_id=v_run.run_id) >= 4096 THEN
        RAISE EXCEPTION USING ERRCODE='K1018',MESSAGE='action_receipt_limit_exceeded';
    END IF;

    INSERT INTO agent_store.actions (
        run_id,action_key,request_hash,episode_hash,tool_call_id,capability_id,
        input_schema_hash,output_schema_hash,data_release_hash,arguments_artifact_ref,
        retryable_read,stage
    ) VALUES (
        v_run.run_id,p_request->>'action_key',p_request->>'request_hash',
        p_request->>'episode_hash',p_request->>'tool_call_id',p_request->>'capability_id',
        p_request->>'input_schema_hash',p_request->>'output_schema_hash',
        p_request->>'data_release_hash',p_request->>'arguments_artifact_ref',
        agent_store.request_boolean(p_request,'retryable_read'),'begun'
    ) RETURNING * INTO v_action;
    UPDATE agent_store.runs
       SET run_version=run_version+1,action_frontier_seq=action_frontier_seq+1,
           action_frontier_hash=agent_store.advance_action_frontier(
               action_frontier_hash,p_request->>'mutation_hash'
           ),updated_at=clock_timestamp()
     WHERE run_id = v_run.run_id RETURNING * INTO v_run;
    v_response := jsonb_build_object(
        'outcome','begun','run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'action_key',v_action.action_key,'stage',v_action.stage,
        'request_hash',v_action.request_hash,'result_hash',v_action.result_hash,
        'retryable_read',v_action.retryable_read,
        'action_frontier_seq',v_run.action_frontier_seq,
        'action_frontier_hash',v_run.action_frontier_hash
    );
    PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','begin_action',p_request->>'mutation_hash',p_request,v_response);
    RETURN v_response;
END;
$$;

COMMIT;
