-- The first bounded-child reservation has no predecessor receipt hash.
-- Keep the field in the request contract, but allow it to be JSON null so the
-- function can distinguish reservation from a later compare-and-swap step.

BEGIN;

CREATE OR REPLACE FUNCTION agent_v1.checkpoint_child_execution(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_child agent_store.child_executions%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
    v_expected_hash text;
    v_target_stage text;
    v_outcome text;
    v_changed boolean := false;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','expected_cancel_generation','child_id',
              'expected_receipt_hash','target_stage','receipt_hash','receipt'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','expected_cancel_generation','child_id',
              'target_stage','receipt_hash','receipt']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'receipt_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'child_id',160);
    PERFORM agent_store.assert_json_size(p_request->'receipt',1048576);
    v_target_stage := p_request->>'target_stage';
    v_expected_hash := p_request->>'expected_receipt_hash';
    IF v_target_stage NOT IN ('reserved','invoked','completed','cancelled')
       OR p_request->'receipt'->>'run_id' IS DISTINCT FROM p_request->>'run_id'
       OR p_request->'receipt'->>'child_id' IS DISTINCT FROM p_request->>'child_id'
       OR p_request->'receipt'->>'stage' IS DISTINCT FROM v_target_stage
       OR p_request->'receipt'->>'fencing_token' IS DISTINCT FROM p_request->>'fencing_token'
       OR p_request->'receipt'->>'cancel_generation' IS DISTINCT FROM p_request->>'expected_cancel_generation'
       OR p_request->'receipt' ? 'transcript'
       OR p_request->'receipt' ? 'messages'
       OR p_request->'receipt' ? 'reasoning_content' THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='invalid_child_receipt';
    END IF;
    IF v_expected_hash IS NOT NULL THEN
        PERFORM agent_store.assert_hash(v_expected_hash);
    END IF;

    SELECT * INTO v_run FROM agent_store.runs
     WHERE run_id=p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch';
    END IF;
    IF v_run.fencing_token<>agent_store.request_bigint(p_request,'fencing_token',0,9223372036854775807) THEN
        RAISE EXCEPTION USING ERRCODE='K1002',MESSAGE='stale_fence';
    END IF;
    v_replay:=agent_store.replay_mutation(
        v_run.run_id,p_request->>'mutation_id','checkpoint_child_execution',
        p_request->>'mutation_hash',p_request
    );
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state<>'active' THEN
        RAISE EXCEPTION USING ERRCODE='K1003',MESSAGE='terminal_or_inactive_run';
    END IF;
    IF v_run.lease_deadline<=clock_timestamp() THEN
        RAISE EXCEPTION USING ERRCODE='K1013',MESSAGE='lease_lost';
    END IF;
    IF v_run.run_version<>agent_store.request_bigint(p_request,'expected_run_version',1,9223372036854775807) THEN
        RAISE EXCEPTION USING ERRCODE='K1012',MESSAGE='run_version_mismatch';
    END IF;
    IF v_run.cancel_generation<>agent_store.request_bigint(p_request,'expected_cancel_generation',0,9223372036854775807) THEN
        RAISE EXCEPTION USING ERRCODE='K1009',MESSAGE='cancel_generation_mismatch';
    END IF;

    SELECT * INTO v_child FROM agent_store.child_executions
     WHERE run_id=v_run.run_id FOR UPDATE;
    IF NOT FOUND THEN
        IF v_expected_hash IS NOT NULL OR v_target_stage<>'reserved' THEN
            RAISE EXCEPTION USING ERRCODE='K1024',MESSAGE='child_reservation_missing';
        END IF;
        INSERT INTO agent_store.child_executions(
            run_id,child_id,stage,receipt_hash,receipt
        ) VALUES (
            v_run.run_id,p_request->>'child_id',v_target_stage,
            p_request->>'receipt_hash',p_request->'receipt'
        ) RETURNING * INTO v_child;
        v_outcome := 'reserved';
        v_changed := true;
    ELSIF v_child.receipt_hash = p_request->>'receipt_hash' THEN
        IF v_child.child_id IS DISTINCT FROM p_request->>'child_id'
           OR v_child.stage IS DISTINCT FROM v_target_stage
           OR v_child.receipt IS DISTINCT FROM p_request->'receipt' THEN
            RAISE EXCEPTION USING ERRCODE='K1024',MESSAGE='child_receipt_hash_conflict';
        END IF;
        v_outcome := 'already_' || v_target_stage;
    ELSE
        IF v_child.child_id IS DISTINCT FROM p_request->>'child_id'
           OR v_expected_hash IS NULL
           OR v_child.receipt_hash IS DISTINCT FROM v_expected_hash
           OR NOT (
                (v_child.stage='reserved' AND v_target_stage IN ('invoked','cancelled'))
                OR (v_child.stage='invoked' AND v_target_stage IN ('completed','cancelled'))
           ) THEN
            RAISE EXCEPTION USING ERRCODE='K1024',MESSAGE='child_transition_conflict';
        END IF;
        UPDATE agent_store.child_executions
           SET stage=v_target_stage,
               receipt_hash=p_request->>'receipt_hash',
               receipt=p_request->'receipt',
               updated_at=clock_timestamp()
         WHERE run_id=v_run.run_id
         RETURNING * INTO v_child;
        v_outcome := v_target_stage;
        v_changed := true;
    END IF;

    IF v_changed THEN
        UPDATE agent_store.runs
           SET run_version=run_version+1,updated_at=clock_timestamp()
         WHERE run_id=v_run.run_id RETURNING * INTO v_run;
    END IF;
    v_response:=jsonb_build_object(
        'outcome',v_outcome,'run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'child_id',v_child.child_id,'stage',v_child.stage,
        'receipt_hash',v_child.receipt_hash,'receipt',v_child.receipt
    );
    PERFORM agent_store.record_mutation(
        v_run.run_id,p_request->>'mutation_id','checkpoint_child_execution',
        p_request->>'mutation_hash',p_request,v_response
    );
    RETURN v_response;
END;
$$;

REVOKE ALL ON FUNCTION agent_v1.checkpoint_child_execution(jsonb) FROM PUBLIC;

COMMIT;
