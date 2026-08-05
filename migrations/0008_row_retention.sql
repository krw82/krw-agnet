BEGIN;

-- Reap terminal runs older than a retention cutoff, deleting their full
-- child row graph in FK order within a single transaction. Bounded by
-- max_runs per invocation so the reaper never blocks the queue for long.
--
-- Scope note: this deletes only run-owned rows. The session-scoped memory
-- projections (agent_store.session_memory_frontiers, session_memory_snapshots,
-- and session_memory_deltas) are intentionally NOT touched:
--   * frontiers/snapshots are keyed by (tenant, principal, session), not
--     run_id, and survive across many runs in the same session;
--   * session_memory_deltas carry a BEFORE DELETE trigger
--     (reject_session_memory_delta_mutation) that raises K1025, so they are
--     append-only by design and must not be reaped here.
-- session_memory_snapshot_mutations IS run-owned and is removed.
CREATE FUNCTION agent_v1.reap_retained_runs(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_cutoff timestamptz;
    v_max bigint;
    v_reaped bigint := 0;
    v_run record;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','worker_id','retention_days','max_runs'],
        ARRAY['abi_version','worker_id','retention_days','max_runs']
    );
    PERFORM agent_store.assert_text(p_request,'worker_id',128);
    v_cutoff := clock_timestamp()
        - (agent_store.request_bigint(p_request,'retention_days',1,3650) * interval '1 day');
    v_max := agent_store.request_bigint(p_request,'max_runs',1,10000);
    FOR v_run IN
        SELECT run_id FROM agent_store.runs
         WHERE state IN ('final','failed','cancelled')
           AND updated_at < v_cutoff
         ORDER BY updated_at
         LIMIT v_max
         FOR UPDATE SKIP LOCKED
    LOOP
        -- Delete in FK dependency order (children before parents). Every
        -- target below carries a run_id column; rows without run_id are
        -- omitted (see function header).
        DELETE FROM agent_store.session_memory_snapshot_mutations WHERE run_id = v_run.run_id;
        DELETE FROM agent_store.outbox WHERE run_id = v_run.run_id;
        DELETE FROM agent_store.settlements WHERE run_id = v_run.run_id;
        DELETE FROM agent_store.answer_bundles WHERE run_id = v_run.run_id;
        DELETE FROM agent_store.actions WHERE run_id = v_run.run_id;
        DELETE FROM agent_store.provider_episodes WHERE run_id = v_run.run_id;
        DELETE FROM agent_store.run_state_checkpoints WHERE run_id = v_run.run_id;
        DELETE FROM agent_store.child_executions WHERE run_id = v_run.run_id;
        DELETE FROM agent_store.mutations WHERE run_id = v_run.run_id;
        DELETE FROM agent_store.runs WHERE run_id = v_run.run_id;
        v_reaped := v_reaped + 1;
    END LOOP;
    RETURN jsonb_build_object('reaped', v_reaped, 'cutoff', v_cutoff);
END;
$$;

GRANT EXECUTE ON FUNCTION agent_v1.reap_retained_runs(jsonb) TO krw_agent_daemon;

COMMIT;
