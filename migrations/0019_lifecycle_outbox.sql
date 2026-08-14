-- Durable, content-free lifecycle projection for product chat.
--
-- These events are optional UI telemetry.  The helper deliberately swallows
-- insertion errors so a projection outage can never abort a research action,
-- checkpoint, or final answer.  Terminal outbox events remain authoritative.

BEGIN;

CREATE OR REPLACE FUNCTION agent_store.enqueue_lifecycle_event(
    p_run_id text,
    p_run_version bigint,
    p_stage text
) RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_event_kind text;
    v_dedupe_key text;
    v_payload jsonb;
BEGIN
    IF p_stage NOT IN ('started', 'researching', 'composing')
       OR p_run_version < 1 THEN
        RETURN;
    END IF;

    v_event_kind := CASE WHEN p_stage = 'started'
                         THEN 'run.started' ELSE 'run.progress' END;
    v_dedupe_key := p_run_id || ':' || p_stage;
    v_payload := jsonb_build_object(
        'schema_version', 1,
        'run_id', p_run_id,
        'run_version', p_run_version,
        'occurred_at', clock_timestamp()
    );
    IF p_stage <> 'started' THEN
        v_payload := v_payload || jsonb_build_object('stage', p_stage);
    END IF;

    INSERT INTO agent_store.outbox (run_id, event_kind, dedupe_key, payload)
    VALUES (p_run_id, v_event_kind, v_dedupe_key, v_payload)
    ON CONFLICT (event_kind, dedupe_key) DO NOTHING;
EXCEPTION
    WHEN OTHERS THEN
        -- Lifecycle projection is never allowed to turn a valid research
        -- transaction into a failed run.  The terminal event remains enough
        -- for product reconciliation when this best-effort insert is lost.
        RETURN;
END;
$$;

REVOKE ALL ON FUNCTION agent_store.enqueue_lifecycle_event(text, bigint, text)
    FROM PUBLIC;

CREATE OR REPLACE FUNCTION agent_store.emit_started_after_claim()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
BEGIN
    IF OLD.state IS DISTINCT FROM 'active' AND NEW.state = 'active' THEN
        PERFORM agent_store.enqueue_lifecycle_event(
            NEW.run_id, NEW.run_version, 'started'
        );
    END IF;
    RETURN NEW;
END;
$$;

REVOKE ALL ON FUNCTION agent_store.emit_started_after_claim() FROM PUBLIC;

DROP TRIGGER IF EXISTS trg_agent_store_lifecycle_started ON agent_store.runs;
CREATE TRIGGER trg_agent_store_lifecycle_started
    AFTER UPDATE OF state ON agent_store.runs
    FOR EACH ROW
    EXECUTE FUNCTION agent_store.emit_started_after_claim();

CREATE OR REPLACE FUNCTION agent_store.emit_researching_after_action()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run_version bigint;
BEGIN
    IF OLD.stage IS DISTINCT FROM 'accepted' AND NEW.stage = 'accepted' THEN
        SELECT run_version INTO v_run_version
          FROM agent_store.runs
         WHERE run_id = NEW.run_id;
        IF FOUND THEN
            PERFORM agent_store.enqueue_lifecycle_event(
                NEW.run_id, v_run_version, 'researching'
            );
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

REVOKE ALL ON FUNCTION agent_store.emit_researching_after_action() FROM PUBLIC;

DROP TRIGGER IF EXISTS trg_agent_store_lifecycle_researching ON agent_store.actions;
CREATE TRIGGER trg_agent_store_lifecycle_researching
    AFTER UPDATE OF stage ON agent_store.actions
    FOR EACH ROW
    EXECUTE FUNCTION agent_store.emit_researching_after_action();

-- The existing checkpoint ABI is retained; the optional lifecycle_stage field
-- only causes a composing event and is still included in the normal mutation
-- hash/replay contract.  This avoids an extra Rust↔Postgres round trip.
CREATE OR REPLACE FUNCTION agent_v1.checkpoint_run_state(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_existing agent_store.run_state_checkpoints%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
    v_provider_checkpoint_seq bigint;
    v_action_frontier_seq bigint;
    v_state_size_bytes bigint;
    v_outcome text;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','provider_checkpoint_seq','action_frontier_seq',
              'action_frontier_hash','recovery_schema_hash','state_hash','state_artifact_ref',
              'state_size_bytes','lifecycle_stage'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','provider_checkpoint_seq','action_frontier_seq',
              'action_frontier_hash','recovery_schema_hash','state_hash','state_artifact_ref',
              'state_size_bytes']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'action_frontier_hash');
    PERFORM agent_store.assert_hash(p_request->>'recovery_schema_hash');
    PERFORM agent_store.assert_hash(p_request->>'state_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'state_artifact_ref',2048);
    IF p_request->>'lifecycle_stage' IS NOT NULL
       AND p_request->>'lifecycle_stage' <> 'composing' THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='invalid_lifecycle_stage';
    END IF;
    v_provider_checkpoint_seq := agent_store.request_bigint(
        p_request,'provider_checkpoint_seq',0,9223372036854775807
    );
    v_action_frontier_seq := agent_store.request_bigint(
        p_request,'action_frontier_seq',0,9223372036854775807
    );
    v_state_size_bytes := agent_store.request_bigint(
        p_request,'state_size_bytes',1,8388608
    );

    SELECT * INTO v_run FROM agent_store.runs
     WHERE run_id=p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run';
    END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch';
    END IF;
    IF v_run.fencing_token <> agent_store.request_bigint(
        p_request,'fencing_token',0,9223372036854775807
    ) THEN
        RAISE EXCEPTION USING ERRCODE='K1002',MESSAGE='stale_fence';
    END IF;
    v_replay := agent_store.replay_mutation(
        v_run.run_id,p_request->>'mutation_id','checkpoint_run_state',
        p_request->>'mutation_hash',p_request
    );
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state <> 'active' THEN
        RAISE EXCEPTION USING ERRCODE='K1003',MESSAGE='terminal_or_inactive_run';
    END IF;
    IF v_run.lease_deadline <= clock_timestamp() THEN
        RAISE EXCEPTION USING ERRCODE='K1013',MESSAGE='lease_lost';
    END IF;
    IF v_run.run_version <> agent_store.request_bigint(
        p_request,'expected_run_version',1,9223372036854775807
    ) THEN
        RAISE EXCEPTION USING ERRCODE='K1012',MESSAGE='run_version_mismatch';
    END IF;
    IF v_provider_checkpoint_seq <> v_run.checkpoint_seq THEN
        RAISE EXCEPTION USING ERRCODE='K1015',MESSAGE='checkpoint_sequence_mismatch';
    END IF;
    IF v_action_frontier_seq <> v_run.action_frontier_seq
       OR p_request->>'action_frontier_hash' IS DISTINCT FROM v_run.action_frontier_hash THEN
        RAISE EXCEPTION USING ERRCODE='K1021',MESSAGE='action_frontier_mismatch';
    END IF;
    IF (
        SELECT count(*) FROM agent_store.mutations
         WHERE run_id=v_run.run_id AND operation='checkpoint_run_state'
    ) >= 4096 THEN
        RAISE EXCEPTION USING ERRCODE='K1018',MESSAGE='runtime_state_checkpoint_limit_exceeded';
    END IF;

    SELECT * INTO v_existing FROM agent_store.run_state_checkpoints
     WHERE run_id=v_run.run_id;
    IF FOUND
       AND v_existing.state_hash IS NOT DISTINCT FROM p_request->>'state_hash'
       AND v_existing.state_artifact_ref IS NOT DISTINCT FROM p_request->>'state_artifact_ref'
       AND v_existing.state_size_bytes = v_state_size_bytes
       AND v_existing.recovery_schema_hash IS NOT DISTINCT FROM p_request->>'recovery_schema_hash'
       AND v_existing.provider_checkpoint_seq = v_provider_checkpoint_seq
       AND v_existing.action_frontier_seq = v_action_frontier_seq
       AND v_existing.action_frontier_hash IS NOT DISTINCT FROM p_request->>'action_frontier_hash' THEN
        v_outcome := 'already_checkpointed';
    ELSE
        INSERT INTO agent_store.run_state_checkpoints (
            run_id,state_hash,state_artifact_ref,state_size_bytes,recovery_schema_hash,
            provider_checkpoint_seq,action_frontier_seq,action_frontier_hash
        ) VALUES (
            v_run.run_id,p_request->>'state_hash',p_request->>'state_artifact_ref',
            v_state_size_bytes,p_request->>'recovery_schema_hash',v_provider_checkpoint_seq,
            v_action_frontier_seq,p_request->>'action_frontier_hash'
        )
        ON CONFLICT (run_id) DO UPDATE SET
            state_hash=EXCLUDED.state_hash,
            state_artifact_ref=EXCLUDED.state_artifact_ref,
            state_size_bytes=EXCLUDED.state_size_bytes,
            recovery_schema_hash=EXCLUDED.recovery_schema_hash,
            provider_checkpoint_seq=EXCLUDED.provider_checkpoint_seq,
            action_frontier_seq=EXCLUDED.action_frontier_seq,
            action_frontier_hash=EXCLUDED.action_frontier_hash,
            committed_at=clock_timestamp();
        UPDATE agent_store.runs
           SET run_version=run_version+1,updated_at=clock_timestamp()
         WHERE run_id=v_run.run_id RETURNING * INTO v_run;
        v_outcome := 'checkpointed';
    END IF;

    IF p_request->>'lifecycle_stage' = 'composing' THEN
        PERFORM agent_store.enqueue_lifecycle_event(
            v_run.run_id, v_run.run_version, 'composing'
        );
    END IF;

    v_response := jsonb_build_object(
        'outcome',v_outcome,'run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'provider_checkpoint_seq',v_provider_checkpoint_seq,
        'action_frontier_seq',v_action_frontier_seq,
        'action_frontier_hash',v_run.action_frontier_hash,
        'recovery_schema_hash',p_request->>'recovery_schema_hash',
        'state_hash',p_request->>'state_hash','state_size_bytes',v_state_size_bytes
    );
    PERFORM agent_store.record_mutation(
        v_run.run_id,p_request->>'mutation_id','checkpoint_run_state',
        p_request->>'mutation_hash',p_request,v_response
    );
    RETURN v_response;
END;
$$;

REVOKE ALL ON FUNCTION agent_v1.checkpoint_run_state(jsonb) FROM PUBLIC;

COMMIT;
