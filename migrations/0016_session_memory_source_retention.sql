-- Keep the small provenance needed for chat-room follow-ups after a terminal
-- run's provider/action graph is reaped.  The tombstone is deliberately
-- content-free beyond hashes that were already validated by commit_final.

BEGIN;

CREATE TABLE agent_store.session_memory_sources (
    run_id text NOT NULL,
    tenant_id text NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 128),
    principal_id text NOT NULL CHECK (char_length(principal_id) BETWEEN 1 AND 128),
    session_id text NOT NULL CHECK (char_length(session_id) BETWEEN 1 AND 128),
    answer_bundle_hash text CHECK (answer_bundle_hash IS NULL OR answer_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
    final_output_hash text CHECK (final_output_hash IS NULL OR final_output_hash ~ '^sha256:[0-9a-f]{64}$'),
    final_commit_intent_hash text CHECK (final_commit_intent_hash IS NULL OR final_commit_intent_hash ~ '^sha256:[0-9a-f]{64}$'),
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (run_id, tenant_id, principal_id, session_id),
    UNIQUE (run_id)
);

REVOKE ALL ON TABLE agent_store.session_memory_sources FROM PUBLIC;

-- Backfill every source referenced by a delta or snapshot before replacing the
-- run foreign keys.  A source may be present only as a snapshot checkpoint,
-- therefore answer-bundle hashes are optional for that legacy row.
INSERT INTO agent_store.session_memory_sources(
    run_id, tenant_id, principal_id, session_id,
    answer_bundle_hash, final_output_hash, final_commit_intent_hash
)
SELECT DISTINCT ON (source_run_id, tenant_id, principal_id, session_id)
    source_run_id, tenant_id, principal_id, session_id,
    NULLIF(delta->'source'->>'answer_bundle_hash',''),
    NULLIF(delta->'source'->>'final_output_hash',''),
    NULLIF(delta->'source'->>'final_commit_intent_hash','')
FROM agent_store.session_memory_deltas
ORDER BY source_run_id, tenant_id, principal_id, session_id, revision DESC
ON CONFLICT (run_id) DO NOTHING;

INSERT INTO agent_store.session_memory_sources(
    run_id, tenant_id, principal_id, session_id, answer_bundle_hash, final_output_hash
)
SELECT DISTINCT s.checkpoint_run_id, s.tenant_id, s.principal_id, s.session_id,
       b.answer_bundle_hash, b.final_output_hash
FROM agent_store.session_memory_snapshots s
LEFT JOIN agent_store.answer_bundles b ON b.run_id = s.checkpoint_run_id
ON CONFLICT (run_id) DO NOTHING;

-- The original migrations used run-owned FKs for these session projections.
-- Discover their generated names instead of relying on PostgreSQL's truncation
-- rules, then replace them with the immutable tombstone owner.
DO $migration$
DECLARE
    constraint_row record;
BEGIN
    FOR constraint_row IN
        SELECT conrelid::regclass AS relation, conname
        FROM pg_constraint
        WHERE contype = 'f'
          AND confrelid = 'agent_store.runs'::regclass
          AND conrelid IN (
              'agent_store.session_memory_frontiers'::regclass,
              'agent_store.session_memory_deltas'::regclass,
              'agent_store.session_memory_snapshots'::regclass
          )
    LOOP
        EXECUTE format(
            'ALTER TABLE %s DROP CONSTRAINT %I',
            constraint_row.relation,
            constraint_row.conname
        );
    END LOOP;
END;
$migration$;

ALTER TABLE agent_store.session_memory_frontiers
    ADD CONSTRAINT session_memory_frontiers_source_tombstone_fk
    FOREIGN KEY (last_source_run_id, tenant_id, principal_id, session_id)
    REFERENCES agent_store.session_memory_sources
        (run_id, tenant_id, principal_id, session_id)
    ON DELETE RESTRICT;

ALTER TABLE agent_store.session_memory_deltas
    ADD CONSTRAINT session_memory_deltas_source_tombstone_fk
    FOREIGN KEY (source_run_id, tenant_id, principal_id, session_id)
    REFERENCES agent_store.session_memory_sources
        (run_id, tenant_id, principal_id, session_id)
    ON DELETE RESTRICT;

ALTER TABLE agent_store.session_memory_snapshots
    ADD CONSTRAINT session_memory_snapshots_checkpoint_tombstone_fk
    FOREIGN KEY (checkpoint_run_id, tenant_id, principal_id, session_id)
    REFERENCES agent_store.session_memory_sources
        (run_id, tenant_id, principal_id, session_id)
    ON DELETE RESTRICT;

CREATE OR REPLACE FUNCTION agent_store.reject_session_memory_delta_mutation() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
    IF current_setting('krw.session_memory_maintenance', true) = 'on' THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_delta_is_append_only';
END;
$function$;

CREATE OR REPLACE FUNCTION agent_store.reject_session_memory_snapshot_mutation() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
    IF current_setting('krw.session_memory_maintenance', true) = 'on' THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_snapshot_is_append_only';
END;
$function$;

CREATE FUNCTION agent_store.capture_session_memory_source() RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $function$
BEGIN
    INSERT INTO agent_store.session_memory_sources(
        run_id, tenant_id, principal_id, session_id,
        answer_bundle_hash, final_output_hash, final_commit_intent_hash
    ) VALUES (
        NEW.source_run_id, NEW.tenant_id, NEW.principal_id, NEW.session_id,
        NULLIF(NEW.delta->'source'->>'answer_bundle_hash',''),
        NULLIF(NEW.delta->'source'->>'final_output_hash',''),
        NULLIF(NEW.delta->'source'->>'final_commit_intent_hash','')
    )
    ON CONFLICT (run_id) DO UPDATE SET
        answer_bundle_hash = COALESCE(agent_store.session_memory_sources.answer_bundle_hash,
                                      EXCLUDED.answer_bundle_hash),
        final_output_hash = COALESCE(agent_store.session_memory_sources.final_output_hash,
                                     EXCLUDED.final_output_hash),
        final_commit_intent_hash = COALESCE(agent_store.session_memory_sources.final_commit_intent_hash,
                                            EXCLUDED.final_commit_intent_hash);
    RETURN NEW;
END;
$function$;

-- This must run before the delta's immediate foreign-key check.  The source
-- tombstone is the new parent of the delta, so an AFTER trigger would be too
-- late for PostgreSQL's default (non-deferrable) FK enforcement.
CREATE TRIGGER session_memory_source_tombstone_before_insert
    BEFORE INSERT ON agent_store.session_memory_deltas
    FOR EACH ROW EXECUTE FUNCTION agent_store.capture_session_memory_source();

-- A trusted product hard-purge is the only operation allowed to delete the
-- context projection. Soft delete, logout, inactivity, and ordinary reaping
-- never call this procedure.
CREATE TABLE agent_store.session_memory_retirements (
    tenant_id text NOT NULL,
    principal_id text NOT NULL,
    session_id text NOT NULL,
    mutation_id text NOT NULL CHECK (char_length(mutation_id) BETWEEN 1 AND 128),
    mutation_hash text NOT NULL CHECK (mutation_hash ~ '^sha256:[0-9a-f]{64}$'),
    lifecycle_receipt_hash text NOT NULL CHECK (lifecycle_receipt_hash ~ '^sha256:[0-9a-f]{64}$'),
    response_payload jsonb NOT NULL,
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, principal_id, session_id, mutation_id)
);

CREATE FUNCTION agent_v1.retire_session_memory(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $function$
DECLARE
    v_existing agent_store.session_memory_retirements%ROWTYPE;
    v_active bigint;
    v_response jsonb;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','tenant_id','principal_id',
              'session_id','lifecycle_receipt_hash','reason_code'],
        ARRAY['abi_version','mutation_id','mutation_hash','tenant_id','principal_id',
              'session_id','lifecycle_receipt_hash','reason_code']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'lifecycle_receipt_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'principal_id',128);
    PERFORM agent_store.assert_text(p_request,'session_id',128);
    IF p_request->>'reason_code' <> 'product_hard_purge' THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='invalid_retirement_reason';
    END IF;

    SELECT * INTO v_existing
      FROM agent_store.session_memory_retirements
     WHERE tenant_id=p_request->>'tenant_id'
       AND principal_id=p_request->>'principal_id'
       AND session_id=p_request->>'session_id'
       AND mutation_id=p_request->>'mutation_id';
    IF FOUND THEN
        IF v_existing.mutation_hash IS DISTINCT FROM p_request->>'mutation_hash'
           OR v_existing.lifecycle_receipt_hash IS DISTINCT FROM p_request->>'lifecycle_receipt_hash' THEN
            RAISE EXCEPTION USING ERRCODE='K1004',MESSAGE='retirement_mutation_conflict';
        END IF;
        RETURN v_existing.response_payload;
    END IF;

    PERFORM pg_advisory_xact_lock(
        hashtextextended(
            p_request->>'tenant_id' || chr(0) || p_request->>'principal_id' || chr(0) || p_request->>'session_id',
            0
        )
    );
    SELECT count(*) INTO v_active
      FROM agent_store.runs
     WHERE tenant_id=p_request->>'tenant_id'
       AND principal_id=p_request->>'principal_id'
       AND session_id=p_request->>'session_id'
       AND state IN ('queued','deferred','active');
    IF v_active <> 0 THEN
        RAISE EXCEPTION USING ERRCODE='K1024',MESSAGE='session_memory_retirement_busy';
    END IF;

    PERFORM set_config('krw.session_memory_maintenance','on',true);
    DELETE FROM agent_store.session_memory_snapshot_mutations m
     USING agent_store.runs r
     WHERE m.run_id=r.run_id
       AND r.tenant_id=p_request->>'tenant_id'
       AND r.principal_id=p_request->>'principal_id'
       AND r.session_id=p_request->>'session_id';
    DELETE FROM agent_store.session_memory_snapshots
     WHERE tenant_id=p_request->>'tenant_id'
       AND principal_id=p_request->>'principal_id'
       AND session_id=p_request->>'session_id';
    DELETE FROM agent_store.session_memory_deltas
     WHERE tenant_id=p_request->>'tenant_id'
       AND principal_id=p_request->>'principal_id'
       AND session_id=p_request->>'session_id';
    DELETE FROM agent_store.session_memory_frontiers
     WHERE tenant_id=p_request->>'tenant_id'
       AND principal_id=p_request->>'principal_id'
       AND session_id=p_request->>'session_id';
    DELETE FROM agent_store.session_memory_sources
     WHERE tenant_id=p_request->>'tenant_id'
       AND principal_id=p_request->>'principal_id'
       AND session_id=p_request->>'session_id';

    v_response := jsonb_build_object(
        'outcome','retired',
        'tenant_id',p_request->>'tenant_id',
        'principal_id',p_request->>'principal_id',
        'session_id',p_request->>'session_id',
        'lifecycle_receipt_hash',p_request->>'lifecycle_receipt_hash'
    );
    INSERT INTO agent_store.session_memory_retirements(
        tenant_id,principal_id,session_id,mutation_id,mutation_hash,
        lifecycle_receipt_hash,response_payload
    ) VALUES (
        p_request->>'tenant_id',p_request->>'principal_id',p_request->>'session_id',
        p_request->>'mutation_id',p_request->>'mutation_hash',
        p_request->>'lifecycle_receipt_hash',v_response
    );
    RETURN v_response;
END;
$function$;

REVOKE ALL ON TABLE agent_store.session_memory_retirements FROM PUBLIC;
REVOKE ALL ON FUNCTION agent_v1.retire_session_memory(jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION agent_v1.retire_session_memory(jsonb) TO krw_agent_daemon;

COMMIT;
