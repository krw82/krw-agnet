BEGIN;

-- 2026-09-08 (sector/multi-ticker deferral incidents): the mutation replay
-- comparison bound the full request bytes, including the lease-scoped
-- fencing_token/expected_run_version. A deferral-resumed run re-issues
-- begin_action with the same mutation id but a fresh lease (every re-claim
-- advances the fence), so replay died as K1004 mutation_conflict forever —
-- the executor retried, each retry re-leased and re-conflicted, and the
-- run escaped to the ledger fallback with zero new reads. Bind replay
-- identity to the stable payload (request minus the two lease fields) and
-- refresh the replayed response's lease fields to the current fence so
-- callers that validate the response against the CURRENT fence accept it.
-- The payload_hash parameter stays in the signature for compatibility but
-- no longer participates: it binds the full request bytes, which is exactly
-- what must not define identity here.

CREATE OR REPLACE FUNCTION agent_store.replay_mutation(
    p_run_id text,
    p_mutation_id text,
    p_operation text,
    p_payload_hash text,
    p_request_payload jsonb
) RETURNS jsonb
LANGUAGE plpgsql
VOLATILE
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_stored agent_store.mutations%ROWTYPE;
BEGIN
    SELECT * INTO v_stored
      FROM agent_store.mutations
     WHERE run_id = p_run_id AND mutation_id = p_mutation_id;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;
    IF v_stored.operation IS DISTINCT FROM p_operation
       OR (v_stored.request_payload - 'fencing_token' - 'expected_run_version')
          IS DISTINCT FROM
          (p_request_payload - 'fencing_token' - 'expected_run_version') THEN
        RAISE EXCEPTION USING ERRCODE = 'K1004', MESSAGE = 'mutation_conflict';
    END IF;
    IF v_stored.response_payload ? 'fencing_token' THEN
        RETURN (
            SELECT v_stored.response_payload - 'fencing_token' - 'run_version'
                 || jsonb_build_object(
                        'fencing_token', r.fencing_token,
                        'run_version', r.run_version
                    )
              FROM agent_store.runs r
             WHERE r.run_id = p_run_id
        );
    END IF;
    RETURN v_stored.response_payload;
END;
$$;

COMMIT;
