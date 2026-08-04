-- Irreversible action-finalization ABI cutover.
--
-- This project has no pre-finalization runtime compatibility contract. The
-- only successful path is observe_action followed by finalize_action with an
-- explicit accepted/rejected disposition and both deterministic receipts.

BEGIN;

LOCK TABLE agent_store.actions IN ACCESS EXCLUSIVE MODE;

DO $migration$
BEGIN
    IF EXISTS (
        SELECT 1 FROM agent_store.actions WHERE stage = 'committed'
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = 'K1023',
            MESSAGE = 'pre_finalization_action_state_is_not_migratable';
    END IF;
END;
$migration$;

DROP FUNCTION IF EXISTS agent_v1.commit_action(jsonb);

ALTER TABLE agent_store.actions
    ADD COLUMN IF NOT EXISTS validation_receipt_hash text,
    ADD COLUMN IF NOT EXISTS policy_receipt_hash text,
    ADD COLUMN IF NOT EXISTS finalized_at timestamptz,
    DROP COLUMN IF EXISTS committed_at;

-- The initial migration intentionally used generated names for its
-- stage-dependent checks. Drop only checks that mention `stage`, leaving all
-- identifier, hash-format, and artifact-reference bounds intact.
DO $migration$
DECLARE
    v_constraint record;
BEGIN
    FOR v_constraint IN
        SELECT conname
          FROM pg_constraint
         WHERE conrelid = 'agent_store.actions'::regclass
           AND contype = 'c'
           AND pg_get_constraintdef(oid) ~ '\mstage\M'
    LOOP
        EXECUTE format(
            'ALTER TABLE agent_store.actions DROP CONSTRAINT %I',
            v_constraint.conname
        );
    END LOOP;
END;
$migration$;

ALTER TABLE agent_store.actions
    ADD CONSTRAINT actions_stage_v2_check
        CHECK (stage IN ('begun', 'observed', 'accepted', 'rejected', 'ambiguous')),
    ADD CONSTRAINT actions_result_stage_v2_check
        CHECK (
            (stage IN ('observed', 'accepted', 'rejected'))
            = (result_hash IS NOT NULL)
        ),
    ADD CONSTRAINT actions_ambiguous_stage_v2_check
        CHECK ((stage = 'ambiguous') = (ambiguous_reason_code IS NOT NULL)),
    ADD CONSTRAINT actions_validation_receipt_hash_v2_check
        CHECK (
            validation_receipt_hash IS NULL
            OR validation_receipt_hash ~ '^sha256:[0-9a-f]{64}$'
        ),
    ADD CONSTRAINT actions_policy_receipt_hash_v2_check
        CHECK (
            policy_receipt_hash IS NULL
            OR policy_receipt_hash ~ '^sha256:[0-9a-f]{64}$'
        ),
    ADD CONSTRAINT actions_finalization_receipts_v2_check
        CHECK (
            (stage IN ('accepted', 'rejected'))
            = (
                validation_receipt_hash IS NOT NULL
                AND policy_receipt_hash IS NOT NULL
                AND finalized_at IS NOT NULL
            )
        );

DROP INDEX IF EXISTS agent_store.actions_recovery_idx;
CREATE INDEX actions_recovery_idx
    ON agent_store.actions (run_id, stage)
    WHERE stage IN ('begun', 'observed', 'ambiguous');

CREATE OR REPLACE FUNCTION agent_v1.finalize_action(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_action agent_store.actions%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
    v_outcome text;
    v_target_stage text;
    v_changed boolean := false;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','action_key','result_hash','disposition',
              'validation_receipt_hash','policy_receipt_hash'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','action_key','result_hash','disposition',
              'validation_receipt_hash','policy_receipt_hash']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'result_hash');
    PERFORM agent_store.assert_hash(p_request->>'validation_receipt_hash');
    PERFORM agent_store.assert_hash(p_request->>'policy_receipt_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'action_key',128);
    PERFORM agent_store.assert_text(p_request,'disposition',16);
    v_target_stage := p_request->>'disposition';
    IF v_target_stage NOT IN ('accepted','rejected') THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='invalid_action_disposition';
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
        v_run.run_id,p_request->>'mutation_id','finalize_action',
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

    SELECT * INTO v_action FROM agent_store.actions
     WHERE run_id=v_run.run_id AND action_key=p_request->>'action_key' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1006',MESSAGE='unknown_action'; END IF;
    IF v_action.result_hash IS DISTINCT FROM p_request->>'result_hash' THEN
        RAISE EXCEPTION USING ERRCODE='K1008',MESSAGE='finalize_without_matching_observation';
    END IF;

    IF v_action.stage IN ('accepted','rejected') THEN
        IF v_action.stage IS DISTINCT FROM v_target_stage
           OR v_action.validation_receipt_hash IS DISTINCT FROM p_request->>'validation_receipt_hash'
           OR v_action.policy_receipt_hash IS DISTINCT FROM p_request->>'policy_receipt_hash' THEN
            RAISE EXCEPTION USING ERRCODE='K1023',MESSAGE='action_finalization_conflict';
        END IF;
        v_outcome := 'already_' || v_action.stage;
    ELSIF v_action.stage='observed' THEN
        UPDATE agent_store.actions
           SET stage=v_target_stage,
               validation_receipt_hash=p_request->>'validation_receipt_hash',
               policy_receipt_hash=p_request->>'policy_receipt_hash',
               finalized_at=clock_timestamp()
         WHERE run_id=v_run.run_id AND action_key=v_action.action_key
         RETURNING * INTO v_action;
        v_outcome := v_target_stage;
        v_changed := true;
    ELSE
        RAISE EXCEPTION USING ERRCODE='K1008',MESSAGE='finalize_without_matching_observation';
    END IF;

    IF v_changed THEN
        UPDATE agent_store.runs
           SET run_version=run_version+1,action_frontier_seq=action_frontier_seq+1,
               action_frontier_hash=agent_store.advance_action_frontier(
                   action_frontier_hash,p_request->>'mutation_hash'
               ),updated_at=clock_timestamp()
         WHERE run_id=v_run.run_id RETURNING * INTO v_run;
    END IF;

    v_response:=jsonb_build_object(
        'outcome',v_outcome,'run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'action_key',v_action.action_key,'stage',v_action.stage,
        'request_hash',v_action.request_hash,'result_hash',v_action.result_hash,
        'retryable_read',v_action.retryable_read,
        'disposition',v_action.stage,
        'validation_receipt_hash',v_action.validation_receipt_hash,
        'policy_receipt_hash',v_action.policy_receipt_hash,
        'action_frontier_seq',v_run.action_frontier_seq,
        'action_frontier_hash',v_run.action_frontier_hash
    );
    PERFORM agent_store.record_mutation(
        v_run.run_id,p_request->>'mutation_id','finalize_action',
        p_request->>'mutation_hash',p_request,v_response
    );
    RETURN v_response;
END;
$$;

CREATE OR REPLACE FUNCTION agent_v1.mark_action_ambiguous(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_action agent_store.actions%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
    v_outcome text;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','action_key','reason_code'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','action_key','reason_code']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'action_key',128);
    PERFORM agent_store.assert_text(p_request,'reason_code',64);
    SELECT * INTO v_run FROM agent_store.runs WHERE run_id=p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch'; END IF;
    IF v_run.fencing_token<>agent_store.request_bigint(p_request,'fencing_token',0,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE='K1002',MESSAGE='stale_fence'; END IF;
    v_replay:=agent_store.replay_mutation(v_run.run_id,p_request->>'mutation_id','mark_action_ambiguous',p_request->>'mutation_hash',p_request);
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state<>'active' THEN RAISE EXCEPTION USING ERRCODE='K1003',MESSAGE='terminal_or_inactive_run'; END IF;
    IF v_run.lease_deadline<=clock_timestamp() THEN RAISE EXCEPTION USING ERRCODE='K1013',MESSAGE='lease_lost'; END IF;
    IF v_run.run_version<>agent_store.request_bigint(p_request,'expected_run_version',1,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE='K1012',MESSAGE='run_version_mismatch'; END IF;
    SELECT * INTO v_action FROM agent_store.actions
     WHERE run_id=v_run.run_id AND action_key=p_request->>'action_key' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1006',MESSAGE='unknown_action'; END IF;
    IF v_action.stage='accepted' THEN
        v_outcome:='already_accepted';
    ELSIF v_action.stage='rejected' THEN
        v_outcome:='already_rejected';
    ELSIF v_action.stage='observed' THEN
        v_outcome:='already_observed';
    ELSIF v_action.stage='ambiguous' THEN
        IF v_action.ambiguous_reason_code IS DISTINCT FROM p_request->>'reason_code' THEN
            RAISE EXCEPTION USING ERRCODE='K1007',MESSAGE='ambiguous_reason_conflict';
        END IF;
        v_outcome:='already_ambiguous';
    ELSE
        UPDATE agent_store.actions
           SET stage='ambiguous',ambiguous_reason_code=p_request->>'reason_code'
         WHERE run_id=v_run.run_id AND action_key=v_action.action_key RETURNING * INTO v_action;
        UPDATE agent_store.runs
           SET run_version=run_version+1,action_frontier_seq=action_frontier_seq+1,
               action_frontier_hash=agent_store.advance_action_frontier(
                   action_frontier_hash,p_request->>'mutation_hash'
               ),updated_at=clock_timestamp()
         WHERE run_id=v_run.run_id RETURNING * INTO v_run;
        v_outcome:='ambiguous';
    END IF;
    v_response:=jsonb_build_object(
        'outcome',v_outcome,'run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'action_key',v_action.action_key,'stage',v_action.stage,
        'request_hash',v_action.request_hash,'result_hash',v_action.result_hash,
        'retryable_read',v_action.retryable_read,
        'disposition',CASE WHEN v_action.stage IN ('accepted','rejected') THEN v_action.stage ELSE NULL END,
        'validation_receipt_hash',v_action.validation_receipt_hash,
        'policy_receipt_hash',v_action.policy_receipt_hash,
        'action_frontier_seq',v_run.action_frontier_seq,
        'action_frontier_hash',v_run.action_frontier_hash
    );
    PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','mark_action_ambiguous',p_request->>'mutation_hash',p_request,v_response);
    RETURN v_response;
END;
$$;

-- Refresh claim recovery projection so observed, accepted, and rejected raw
-- result bytes remain recoverable together with their deterministic receipts.
CREATE OR REPLACE FUNCTION agent_v1.claim_run(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_lease_ms bigint;
    v_reclaimed boolean;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','worker_id','lease_ms','runtime_version','accepted_agent_image_hashes'],
        ARRAY['abi_version','worker_id','lease_ms','runtime_version','accepted_agent_image_hashes']
    );
    PERFORM agent_store.assert_text(p_request,'worker_id',128);
    PERFORM agent_store.assert_text(p_request,'runtime_version',64);
    v_lease_ms := agent_store.request_bigint(p_request,'lease_ms',1000,600000);
    IF v_lease_ms < 1000 OR v_lease_ms > 600000
       OR jsonb_typeof(p_request->'accepted_agent_image_hashes') <> 'array'
       OR jsonb_array_length(p_request->'accepted_agent_image_hashes') = 0
       OR jsonb_array_length(p_request->'accepted_agent_image_hashes') > 64
       OR EXISTS (
           SELECT 1 FROM jsonb_array_elements(p_request->'accepted_agent_image_hashes') AS images(value)
            WHERE jsonb_typeof(images.value) <> 'string'
               OR trim(both '"' from images.value::text) !~ '^sha256:[0-9a-f]{64}$'
       ) THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'invalid_claim_parameters';
    END IF;

    PERFORM clock.virtual_start_tag
      FROM agent_store.fair_queue_clock AS clock
     WHERE clock.singleton
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='fair_queue_clock_missing';
    END IF;

    SELECT r.* INTO v_run
      FROM agent_store.runs AS r
      JOIN agent_store.session_claim_locks AS session_lock
        ON session_lock.tenant_id=r.tenant_id AND session_lock.session_id=r.session_id
     WHERE r.state = 'active'
       AND r.lease_deadline <= clock_timestamp()
       AND r.runtime_version = p_request->>'runtime_version'
       AND r.agent_image_hash IN (
           SELECT jsonb_array_elements_text(p_request->'accepted_agent_image_hashes')
       )
     ORDER BY r.lease_deadline, r.created_at, r.run_id
     FOR UPDATE OF r, session_lock SKIP LOCKED
     LIMIT 1;
    IF NOT FOUND THEN
        SELECT r.* INTO v_run
          FROM agent_store.runs AS r
          JOIN agent_store.session_claim_locks AS session_lock
            ON session_lock.tenant_id=r.tenant_id AND session_lock.session_id=r.session_id
         WHERE r.state IN ('queued', 'deferred')
           AND r.available_at <= clock_timestamp()
           AND r.runtime_version = p_request->>'runtime_version'
           AND r.agent_image_hash IN (
               SELECT jsonb_array_elements_text(p_request->'accepted_agent_image_hashes')
           )
           AND NOT EXISTS (
               SELECT 1 FROM agent_store.runs AS active_run
                WHERE active_run.tenant_id = r.tenant_id
                  AND active_run.session_id = r.session_id
                  AND active_run.state = 'active'
           )
         ORDER BY r.queue_start_tag, r.queue_finish_tag,
                  r.available_at, r.created_at, r.run_id
         FOR UPDATE OF r, session_lock SKIP LOCKED
         LIMIT 1;
    END IF;
    IF NOT FOUND THEN
        RETURN jsonb_build_object('claimed', false, 'receipt', NULL);
    END IF;
    IF (SELECT count(*) FROM agent_store.provider_episodes WHERE run_id = v_run.run_id) > 256
       OR (SELECT count(*) FROM agent_store.actions WHERE run_id = v_run.run_id) > 512 THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'recovery_history_limit_exceeded';
    END IF;
    v_reclaimed := v_run.state = 'active';

    IF NOT v_reclaimed THEN
        UPDATE agent_store.fair_queue_clock
           SET virtual_start_tag = GREATEST(virtual_start_tag, v_run.queue_start_tag)
         WHERE singleton;
    END IF;

    UPDATE agent_store.runs
       SET state = 'active',
           fencing_token = fencing_token + 1,
           run_version = run_version + 1,
           lease_owner = p_request->>'worker_id',
           lease_deadline = clock_timestamp() + v_lease_ms * interval '1 millisecond',
           updated_at = clock_timestamp()
     WHERE run_id = v_run.run_id
     RETURNING * INTO v_run;

    RETURN jsonb_build_object(
        'claimed', true,
        'receipt', jsonb_build_object(
            'run_id', v_run.run_id,
            'tenant_id', v_run.tenant_id,
            'principal_id', v_run.principal_id,
            'session_id', v_run.session_id,
            'fencing_token', v_run.fencing_token,
            'run_version', v_run.run_version,
            'cancel_generation', v_run.cancel_generation,
            'checkpoint_seq', v_run.checkpoint_seq,
            'action_frontier_seq', v_run.action_frontier_seq,
            'action_frontier_hash', v_run.action_frontier_hash,
            'lease_deadline', v_run.lease_deadline,
            'agent_image_hash', v_run.agent_image_hash,
            'runtime_version', v_run.runtime_version,
            'priority', v_run.priority,
            'immutable_snapshot_hash', v_run.immutable_snapshot_hash,
            'immutable_snapshot', v_run.immutable_snapshot,
            'resource_profile', v_run.resource_profile,
            'budgets', v_run.budgets,
            'reclaimed', v_reclaimed,
            'recovery', jsonb_build_object(
                'state_checkpoint', (
                    SELECT jsonb_build_object(
                        'state_hash',state_hash,
                        'state_artifact_ref',state_artifact_ref,
                        'state_size_bytes',state_size_bytes,
                        'recovery_schema_hash',recovery_schema_hash,
                        'provider_checkpoint_seq',provider_checkpoint_seq,
                        'action_frontier_seq',action_frontier_seq,
                        'action_frontier_hash',action_frontier_hash
                    )
                      FROM agent_store.run_state_checkpoints
                     WHERE run_id=v_run.run_id
                ),
                'episodes', COALESCE((
                    SELECT jsonb_agg(jsonb_build_object(
                        'episode_hash',episode_hash,
                        'checkpoint_seq',checkpoint_seq,
                        'episode_artifact_ref',episode_artifact_ref
                    ) ORDER BY checkpoint_seq)
                      FROM agent_store.provider_episodes
                     WHERE run_id=v_run.run_id
                ), '[]'::jsonb),
                'actions', COALESCE((
                    SELECT jsonb_agg(jsonb_build_object(
                        'action_key',action_key,
                        'request_hash',request_hash,
                        'episode_hash',episode_hash,
                        'tool_call_id',tool_call_id,
                        'capability_id',capability_id,
                        'input_schema_hash',input_schema_hash,
                        'output_schema_hash',output_schema_hash,
                        'data_release_hash',data_release_hash,
                        'arguments_artifact_ref',arguments_artifact_ref,
                        'retryable_read',retryable_read,
                        'stage',stage,
                        'result_hash',result_hash,
                        'result_artifact_ref',result_artifact_ref,
                        'ambiguous_reason_code',ambiguous_reason_code,
                        'disposition',CASE
                            WHEN stage IN ('accepted','rejected') THEN stage ELSE NULL END,
                        'validation_receipt_hash',validation_receipt_hash,
                        'policy_receipt_hash',policy_receipt_hash
                    ) ORDER BY begun_at, action_key)
                      FROM agent_store.actions
                     WHERE run_id=v_run.run_id
                ), '[]'::jsonb)
            )
        )
    );
END;
$$;

REVOKE ALL ON FUNCTION agent_v1.finalize_action(jsonb) FROM PUBLIC;

COMMENT ON FUNCTION agent_v1.finalize_action(jsonb) IS
    'Linearizes validated observed action bytes to an immutable accepted or rejected disposition.';
COMMENT ON COLUMN agent_store.actions.validation_receipt_hash IS
    'Canonical hash of the schema, normalization, and provenance validation receipt.';
COMMENT ON COLUMN agent_store.actions.policy_receipt_hash IS
    'Canonical hash of the deny-monotone AfterAction policy receipt.';

COMMIT;
