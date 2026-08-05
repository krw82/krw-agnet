BEGIN;

-- Track how many times a run has been deferred so persistent failures
-- eventually terminate instead of retrying forever.
ALTER TABLE agent_store.runs
    ADD COLUMN IF NOT EXISTS deferred_attempts smallint NOT NULL DEFAULT 0
    CHECK (deferred_attempts >= 0);

-- Bump the fail_or_defer procedure so the defer branch increments the
-- counter and transitions to 'failed' once the cap is exhausted. The body
-- is copied from 0001's definition with only the defer branch changed; the
-- fail branch is byte-identical to the original.
CREATE OR REPLACE FUNCTION agent_v1.fail_or_defer(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
    v_disposition text;
    v_retry_delay_ms bigint;
    v_queue_start_tag bigint;
    v_queue_finish_tag bigint;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','disposition','reason_code','retry_delay_ms','release'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','disposition','reason_code','retry_delay_ms','release']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'disposition',16);
    PERFORM agent_store.assert_text(p_request,'reason_code',64);
    PERFORM agent_store.assert_json_size(p_request->'release',1048576);
    v_disposition:=p_request->>'disposition';
    v_retry_delay_ms:=agent_store.request_bigint(p_request,'retry_delay_ms',0,86400000);
    IF v_disposition NOT IN ('defer','fail') THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='invalid_disposition';
    END IF;

    IF v_disposition='defer' THEN
        PERFORM clock.virtual_start_tag
          FROM agent_store.fair_queue_clock AS clock
         WHERE clock.singleton
         FOR UPDATE;
        IF NOT FOUND THEN
            RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='fair_queue_clock_missing';
        END IF;
    END IF;

    SELECT * INTO v_run FROM agent_store.runs WHERE run_id=p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch'; END IF;
    IF v_run.fencing_token<>agent_store.request_bigint(p_request,'fencing_token',0,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE='K1002',MESSAGE='stale_fence'; END IF;
    v_replay:=agent_store.replay_mutation(v_run.run_id,p_request->>'mutation_id','fail_or_defer',p_request->>'mutation_hash',p_request);
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state<>'active' THEN RAISE EXCEPTION USING ERRCODE='K1003',MESSAGE='terminal_or_inactive_run'; END IF;
    IF v_run.lease_deadline<=clock_timestamp() THEN RAISE EXCEPTION USING ERRCODE='K1013',MESSAGE='lease_lost'; END IF;
    IF v_run.run_version<>agent_store.request_bigint(p_request,'expected_run_version',1,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE='K1012',MESSAGE='run_version_mismatch'; END IF;

    IF v_disposition='defer' THEN
        IF v_run.deferred_attempts >= 16 THEN
            -- The defer cap is exhausted: transition to a terminal failure
            -- instead of re-queuing. Mirrors the explicit fail branch below
            -- but tags the reason so operators can distinguish cap exhaustion.
            UPDATE agent_store.runs
               SET state='failed',run_version=run_version+1,lease_owner=NULL,lease_deadline=NULL,
                   deferred_attempts=v_run.deferred_attempts,
                   terminal_outcome=jsonb_build_object('kind','failed','reason_code','deferred_attempts_exhausted'),
                   updated_at=clock_timestamp()
             WHERE run_id=v_run.run_id RETURNING * INTO v_run;
            INSERT INTO agent_store.settlements(run_id,settlement_kind,settlement_payload)
            VALUES(v_run.run_id,'released',p_request->'release');
            INSERT INTO agent_store.outbox(run_id,event_kind,dedupe_key,payload)
            VALUES(
                v_run.run_id,'run.failed',v_run.run_id||':run.failed',
                jsonb_build_object('run_id',v_run.run_id,'run_version',v_run.run_version,
                                   'reason_code','deferred_attempts_exhausted')
            );
            v_response:=jsonb_build_object(
                'outcome','failed','state',v_run.state,'run_id',v_run.run_id,
                'fencing_token',v_run.fencing_token,'run_version',v_run.run_version,
                'cancel_generation',v_run.cancel_generation,'checkpoint_seq',v_run.checkpoint_seq
            );
        ELSE
            SELECT allocated.queue_start_tag, allocated.queue_finish_tag
              INTO v_queue_start_tag, v_queue_finish_tag
              FROM agent_store.allocate_fair_queue_tag(
                  v_run.tenant_id, v_run.principal_id
              ) AS allocated;
            UPDATE agent_store.runs
               SET state='deferred',run_version=run_version+1,lease_owner=NULL,lease_deadline=NULL,
                   deferred_attempts=deferred_attempts+1,
                   queue_start_tag=v_queue_start_tag,queue_finish_tag=v_queue_finish_tag,
                   available_at=clock_timestamp()+v_retry_delay_ms*interval '1 millisecond',
                   updated_at=clock_timestamp()
             WHERE run_id=v_run.run_id RETURNING * INTO v_run;
            INSERT INTO agent_store.outbox(run_id,event_kind,dedupe_key,payload)
            VALUES(
                v_run.run_id,'run.deferred',v_run.run_id||':run.deferred:'||v_run.run_version,
                jsonb_build_object('run_id',v_run.run_id,'run_version',v_run.run_version,
                                   'reason_code',p_request->>'reason_code','retry_delay_ms',v_retry_delay_ms)
            );
            v_response:=jsonb_build_object(
                'outcome','deferred','state',v_run.state,'run_id',v_run.run_id,
                'fencing_token',v_run.fencing_token,'run_version',v_run.run_version,
                'cancel_generation',v_run.cancel_generation,'checkpoint_seq',v_run.checkpoint_seq
            );
        END IF;
    ELSE
        UPDATE agent_store.runs
           SET state='failed',run_version=run_version+1,lease_owner=NULL,lease_deadline=NULL,
               terminal_outcome=jsonb_build_object('kind','failed','reason_code',p_request->>'reason_code'),
               updated_at=clock_timestamp()
         WHERE run_id=v_run.run_id RETURNING * INTO v_run;
        INSERT INTO agent_store.settlements(run_id,settlement_kind,settlement_payload)
        VALUES(v_run.run_id,'released',p_request->'release');
        INSERT INTO agent_store.outbox(run_id,event_kind,dedupe_key,payload)
        VALUES(
            v_run.run_id,'run.failed',v_run.run_id||':run.failed',
            jsonb_build_object('run_id',v_run.run_id,'run_version',v_run.run_version,
                               'reason_code',p_request->>'reason_code')
        );
        v_response:=jsonb_build_object(
            'outcome','failed','state',v_run.state,'run_id',v_run.run_id,
            'fencing_token',v_run.fencing_token,'run_version',v_run.run_version,
            'cancel_generation',v_run.cancel_generation,'checkpoint_seq',v_run.checkpoint_seq
        );
    END IF;
    PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','fail_or_defer',p_request->>'mutation_hash',p_request,v_response);
    RETURN v_response;
END;
$$;

COMMIT;
