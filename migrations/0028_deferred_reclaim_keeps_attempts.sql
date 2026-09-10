BEGIN;

-- 2026-09-10 (run_1fb583eb): 0010 reset deferred_attempts on EVERY
-- non-active claim, but a deferral re-claim is exactly that (fail_or_defer
-- re-queues the run as 'deferred'), so the counter ping-ponged 0->1->0 and
-- 0007's 16-attempt cap was unreachable. A persistently failing retryable
-- read (finviz screener outage) spun the claim loop forever -- run_version
-- passed 550 in twenty minutes with the run neither progressing nor
-- terminating. The reset now applies only to 'queued' (a genuinely fresh
-- execution), preserving 0010's intent while letting deferrals accumulate
-- to the designed cap (~two minutes of ride-out before the terminal
-- deferred_attempts_exhausted settlement).
--
-- The function below is the LIVE claim_run body verbatim with only that
-- one condition (and its comment) changed.

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
           updated_at = clock_timestamp(),
           -- Reset the per-claim deferred-attempts counter when a non-active
           -- (queued/deferred) run is freshly claimed. Reclaims of an
           -- already-active run keep the counter. See migration header.
           deferred_attempts = CASE WHEN v_run.state = 'deferred' THEN deferred_attempts ELSE 0 END
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

COMMIT;
