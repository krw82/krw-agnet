-- Bounded canonical SessionMemorySnapshotV3 checkpoints. The full delta audit
-- remains append-only; normal reads restore the latest snapshot and replay at
-- most 32 newer deltas. A rare audit_rebuild mode exists only on the same
-- owner/fence-scoped daemon procedure so a missing/stale checkpoint can heal.

BEGIN;

CREATE TABLE agent_store.session_memory_snapshots (
    tenant_id text NOT NULL,
    principal_id text NOT NULL,
    session_id text NOT NULL,
    revision bigint NOT NULL CHECK (revision >= 1),
    frontier_hash text NOT NULL CHECK (frontier_hash ~ '^sha256:[0-9a-f]{64}$'),
    source_lineage_hash text NOT NULL
        CHECK (source_lineage_hash ~ '^sha256:[0-9a-f]{64}$'),
    snapshot_hash text NOT NULL CHECK (snapshot_hash ~ '^sha256:[0-9a-f]{64}$'),
    snapshot_size_bytes bigint NOT NULL CHECK (snapshot_size_bytes BETWEEN 2 AND 4194304),
    snapshot jsonb NOT NULL CHECK (pg_column_size(snapshot) BETWEEN 2 AND 5242880),
    checkpoint_run_id text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, principal_id, session_id, revision),
    UNIQUE (tenant_id, principal_id, session_id, snapshot_hash),
    FOREIGN KEY (tenant_id, principal_id, session_id)
        REFERENCES agent_store.session_memory_frontiers
        (tenant_id, principal_id, session_id) ON DELETE RESTRICT,
    FOREIGN KEY (checkpoint_run_id, tenant_id, principal_id, session_id)
        REFERENCES agent_store.runs (run_id, tenant_id, principal_id, session_id)
        ON DELETE RESTRICT,
    CHECK (jsonb_typeof(snapshot) = 'object'),
    CHECK (snapshot->>'schema_version' = '3'),
    CHECK (snapshot->>'authority' = 'context_only'),
    CHECK (snapshot->>'revision' IS NOT DISTINCT FROM revision::text),
    CHECK (snapshot->>'frontier_hash' IS NOT DISTINCT FROM frontier_hash),
    CHECK (snapshot->>'source_lineage_hash' IS NOT DISTINCT FROM source_lineage_hash)
);

CREATE INDEX session_memory_snapshots_latest_idx
    ON agent_store.session_memory_snapshots
    (tenant_id, principal_id, session_id, revision DESC)
    INCLUDE (frontier_hash, source_lineage_hash, snapshot_hash,
             snapshot_size_bytes, checkpoint_run_id);

CREATE TABLE agent_store.session_memory_snapshot_mutations (
    run_id text NOT NULL REFERENCES agent_store.runs(run_id) ON DELETE RESTRICT,
    mutation_id text NOT NULL CHECK (char_length(mutation_id) BETWEEN 1 AND 128),
    payload_hash text NOT NULL CHECK (payload_hash ~ '^sha256:[0-9a-f]{64}$'),
    snapshot_hash text NOT NULL CHECK (snapshot_hash ~ '^sha256:[0-9a-f]{64}$'),
    response_payload jsonb NOT NULL,
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (run_id, mutation_id)
);

CREATE FUNCTION agent_store.reject_session_memory_snapshot_mutation() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $$
BEGIN
    RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_snapshot_is_append_only';
END;
$$;

CREATE TRIGGER session_memory_snapshots_append_only
    BEFORE UPDATE OR DELETE ON agent_store.session_memory_snapshots
    FOR EACH ROW EXECUTE FUNCTION agent_store.reject_session_memory_snapshot_mutation();

CREATE FUNCTION agent_v1.checkpoint_session_memory_snapshot(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_memory agent_store.session_memory_frontiers%ROWTYPE;
    v_existing agent_store.session_memory_snapshots%ROWTYPE;
    v_mutation agent_store.session_memory_snapshot_mutations%ROWTYPE;
    v_revision bigint;
    v_snapshot_size bigint;
    v_session_id_hash text;
    v_response jsonb;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','principal_id',
              'session_id','fencing_token','expected_run_version','snapshot_revision',
              'frontier_hash','source_lineage_hash','snapshot_hash',
              'snapshot_size_bytes','snapshot'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','principal_id',
              'session_id','fencing_token','expected_run_version','snapshot_revision',
              'frontier_hash','source_lineage_hash','snapshot_hash',
              'snapshot_size_bytes','snapshot']
    );
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'principal_id',128);
    PERFORM agent_store.assert_text(p_request,'session_id',128);
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'frontier_hash');
    PERFORM agent_store.assert_hash(p_request->>'source_lineage_hash');
    PERFORM agent_store.assert_hash(p_request->>'snapshot_hash');
    v_revision := agent_store.request_bigint(
        p_request,'snapshot_revision',1,9223372036854775807
    );
    v_snapshot_size := agent_store.request_bigint(
        p_request,'snapshot_size_bytes',2,4194304
    );
    IF jsonb_typeof(p_request->'snapshot') <> 'object'
       OR pg_column_size(p_request->'snapshot') > 5242880 THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='invalid_session_memory_snapshot_size';
    END IF;

    SELECT * INTO v_run FROM agent_store.runs
     WHERE run_id=p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch';
    END IF;
    IF v_run.principal_id IS DISTINCT FROM p_request->>'principal_id'
       OR v_run.session_id IS DISTINCT FROM p_request->>'session_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1026',MESSAGE='session_memory_owner_mismatch';
    END IF;
    IF v_run.fencing_token<>agent_store.request_bigint(
        p_request,'fencing_token',0,9223372036854775807
    ) THEN
        RAISE EXCEPTION USING ERRCODE='K1002',MESSAGE='stale_fence';
    END IF;
    SELECT * INTO v_mutation
      FROM agent_store.session_memory_snapshot_mutations
     WHERE run_id=v_run.run_id AND mutation_id=p_request->>'mutation_id';
    IF FOUND THEN
        IF v_mutation.payload_hash IS DISTINCT FROM p_request->>'mutation_hash'
           OR v_mutation.snapshot_hash IS DISTINCT FROM p_request->>'snapshot_hash' THEN
            RAISE EXCEPTION USING ERRCODE='K1004',MESSAGE='snapshot_mutation_conflict';
        END IF;
        RETURN v_mutation.response_payload;
    END IF;
    IF v_run.state<>'active' THEN
        RAISE EXCEPTION USING ERRCODE='K1003',MESSAGE='terminal_or_inactive_run';
    END IF;
    IF v_run.lease_deadline<=clock_timestamp() THEN
        RAISE EXCEPTION USING ERRCODE='K1013',MESSAGE='lease_lost';
    END IF;
    IF v_run.run_version<>agent_store.request_bigint(
        p_request,'expected_run_version',1,9223372036854775807
    ) THEN
        RAISE EXCEPTION USING ERRCODE='K1012',MESSAGE='run_version_mismatch';
    END IF;

    SELECT * INTO v_memory FROM agent_store.session_memory_frontiers
     WHERE tenant_id=v_run.tenant_id
       AND principal_id=v_run.principal_id
       AND session_id=v_run.session_id
     FOR SHARE;
    IF NOT FOUND
       OR v_memory.revision IS DISTINCT FROM v_revision
       OR v_memory.frontier_hash IS DISTINCT FROM p_request->>'frontier_hash'
       OR v_memory.source_lineage_hash IS DISTINCT FROM p_request->>'source_lineage_hash'
       OR NOT EXISTS (
           SELECT 1 FROM agent_store.session_memory_deltas
            WHERE tenant_id=v_run.tenant_id
              AND principal_id=v_run.principal_id
              AND session_id=v_run.session_id
              AND revision=v_revision
              AND next_frontier_hash=p_request->>'frontier_hash'
       ) THEN
        RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_snapshot_anchor_conflict';
    END IF;
    v_session_id_hash := 'sha256:' || encode(
        sha256(convert_to(v_run.session_id,'UTF8')),'hex'
    );
    IF p_request->'snapshot'->>'schema_version' IS DISTINCT FROM '3'
       OR p_request->'snapshot'->>'authority' IS DISTINCT FROM 'context_only'
       OR p_request->'snapshot'->>'session_id_hash' IS DISTINCT FROM v_session_id_hash
       OR p_request->'snapshot'->>'revision' IS DISTINCT FROM v_revision::text
       OR p_request->'snapshot'->>'frontier_hash' IS DISTINCT FROM v_memory.frontier_hash
       OR p_request->'snapshot'->>'source_lineage_hash'
            IS DISTINCT FROM v_memory.source_lineage_hash
       OR jsonb_typeof(p_request->'snapshot'->'recent_source_revision_index') <> 'object'
       OR (
           SELECT count(*)
             FROM jsonb_object_keys(
                 p_request->'snapshot'->'recent_source_revision_index'
             ) AS recent_source_hash
       ) <> LEAST(v_revision,64) THEN
        RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_snapshot_identity_mismatch';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM jsonb_each(p_request->'snapshot'->'recent_source_revision_index') AS entry(key,value)
         WHERE entry.key !~ '^sha256:[0-9a-f]{64}$'
            OR jsonb_typeof(entry.value) <> 'number'
            OR (entry.value #>> '{}') !~ '^[0-9]+$'
    ) THEN
        RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_snapshot_recent_source_invalid';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM jsonb_each(p_request->'snapshot'->'recent_source_revision_index') AS entry(key,value)
         WHERE (entry.value #>> '{}')::numeric
                   NOT BETWEEN v_revision-LEAST(v_revision,64)+1 AND v_revision
    ) OR (
        SELECT count(DISTINCT (entry.value #>> '{}')::numeric)
          FROM jsonb_each(p_request->'snapshot'->'recent_source_revision_index') AS entry(key,value)
    ) <> LEAST(v_revision,64) THEN
        RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_snapshot_recent_source_invalid';
    END IF;

    SELECT * INTO v_existing FROM agent_store.session_memory_snapshots
     WHERE tenant_id=v_run.tenant_id
       AND principal_id=v_run.principal_id
       AND session_id=v_run.session_id
       AND revision=v_revision;
    IF FOUND THEN
        IF v_existing.frontier_hash IS DISTINCT FROM p_request->>'frontier_hash'
           OR v_existing.source_lineage_hash IS DISTINCT FROM p_request->>'source_lineage_hash'
           OR v_existing.snapshot_hash IS DISTINCT FROM p_request->>'snapshot_hash'
           OR v_existing.snapshot_size_bytes IS DISTINCT FROM v_snapshot_size
           OR v_existing.snapshot IS DISTINCT FROM p_request->'snapshot' THEN
            RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_snapshot_conflict';
        END IF;
        v_response:=jsonb_build_object(
            'outcome','already_checkpointed','run_id',v_run.run_id,
            'fencing_token',v_run.fencing_token,'run_version',v_run.run_version,
            'snapshot_revision',v_revision,'frontier_hash',v_existing.frontier_hash,
            'source_lineage_hash',v_existing.source_lineage_hash,
            'snapshot_hash',v_existing.snapshot_hash
        );
        INSERT INTO agent_store.session_memory_snapshot_mutations(
            run_id,mutation_id,payload_hash,snapshot_hash,response_payload
        ) VALUES (
            v_run.run_id,p_request->>'mutation_id',p_request->>'mutation_hash',
            p_request->>'snapshot_hash',v_response
        );
        RETURN v_response;
    END IF;
    IF EXISTS (
        SELECT 1 FROM agent_store.session_memory_snapshots
         WHERE tenant_id=v_run.tenant_id
           AND principal_id=v_run.principal_id
           AND session_id=v_run.session_id
           AND revision>v_revision
    ) THEN
        RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_snapshot_regression';
    END IF;

    INSERT INTO agent_store.session_memory_snapshots(
        tenant_id,principal_id,session_id,revision,frontier_hash,source_lineage_hash,
        snapshot_hash,snapshot_size_bytes,snapshot,checkpoint_run_id
    ) VALUES (
        v_run.tenant_id,v_run.principal_id,v_run.session_id,v_revision,
        p_request->>'frontier_hash',p_request->>'source_lineage_hash',
        p_request->>'snapshot_hash',v_snapshot_size,
        p_request->'snapshot',v_run.run_id
    );
    UPDATE agent_store.runs
       SET run_version=run_version+1,updated_at=clock_timestamp()
     WHERE run_id=v_run.run_id RETURNING * INTO v_run;
    v_response:=jsonb_build_object(
        'outcome','snapshot_checkpointed','run_id',v_run.run_id,
        'fencing_token',v_run.fencing_token,'run_version',v_run.run_version,
        'snapshot_revision',v_revision,'frontier_hash',p_request->>'frontier_hash',
        'source_lineage_hash',p_request->>'source_lineage_hash',
        'snapshot_hash',p_request->>'snapshot_hash'
    );
    INSERT INTO agent_store.session_memory_snapshot_mutations(
        run_id,mutation_id,payload_hash,snapshot_hash,response_payload
    ) VALUES (
        v_run.run_id,p_request->>'mutation_id',p_request->>'mutation_hash',
        p_request->>'snapshot_hash',v_response
    );
    RETURN v_response;
END;
$$;

CREATE OR REPLACE FUNCTION agent_v1.read_session_memory(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_memory agent_store.session_memory_frontiers%ROWTYPE;
    v_snapshot agent_store.session_memory_snapshots%ROWTYPE;
    v_after_revision bigint;
    v_effective_after bigint;
    v_limit bigint;
    v_mode text;
    v_deltas jsonb;
    v_snapshot_payload jsonb;
    v_last_revision bigint;
    v_has_more boolean;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','run_id','tenant_id','principal_id','session_id','fencing_token',
              'after_revision','limit','mode'],
        ARRAY['abi_version','run_id','tenant_id','principal_id','session_id','fencing_token',
              'after_revision','limit','mode']
    );
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'principal_id',128);
    PERFORM agent_store.assert_text(p_request,'session_id',128);
    v_after_revision := agent_store.request_bigint(
        p_request,'after_revision',0,9223372036854775807
    );
    v_limit := agent_store.request_bigint(p_request,'limit',1,8);
    v_mode := p_request->>'mode';
    IF v_mode NOT IN ('snapshot_tail','audit_rebuild') THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='invalid_session_memory_read_mode';
    END IF;

    SELECT * INTO v_run FROM agent_store.runs WHERE run_id=p_request->>'run_id';
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch';
    END IF;
    IF v_run.principal_id IS DISTINCT FROM p_request->>'principal_id'
       OR v_run.session_id IS DISTINCT FROM p_request->>'session_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1026',MESSAGE='session_memory_owner_mismatch';
    END IF;
    IF v_run.fencing_token<>agent_store.request_bigint(
        p_request,'fencing_token',0,9223372036854775807
    ) THEN
        RAISE EXCEPTION USING ERRCODE='K1002',MESSAGE='stale_fence';
    END IF;
    IF v_run.state<>'active' THEN
        RAISE EXCEPTION USING ERRCODE='K1003',MESSAGE='terminal_or_inactive_run';
    END IF;
    IF v_run.lease_deadline<=clock_timestamp() THEN
        RAISE EXCEPTION USING ERRCODE='K1013',MESSAGE='lease_lost';
    END IF;

    SELECT * INTO v_memory FROM agent_store.session_memory_frontiers
     WHERE tenant_id=v_run.tenant_id
       AND principal_id=v_run.principal_id
       AND session_id=v_run.session_id;
    IF NOT FOUND THEN
        v_memory.revision := 0;
        v_memory.frontier_hash :=
            'sha256:a4e1b26a1a1e27b135aa73cb802316af633c59cc50e92358bca73d6c4e6a4fbb';
        v_memory.source_lineage_hash :=
            'sha256:e0d62521a0b7c1b8d1f3ba5e090c6a7bd60261c57adae0f7ba496d4b06216d39';
    END IF;
    IF v_after_revision > v_memory.revision THEN
        RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_revision_ahead';
    END IF;

    v_effective_after := v_after_revision;
    v_snapshot_payload := NULL;
    IF v_mode='snapshot_tail' AND v_memory.revision>0 THEN
        SELECT * INTO v_snapshot FROM agent_store.session_memory_snapshots
         WHERE tenant_id=v_run.tenant_id
           AND principal_id=v_run.principal_id
           AND session_id=v_run.session_id
         ORDER BY revision DESC LIMIT 1;
        IF NOT FOUND THEN
            RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_snapshot_required';
        END IF;
        IF v_memory.revision-v_snapshot.revision>32 THEN
            RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_snapshot_tail_exceeded';
        END IF;
        IF v_after_revision=0 THEN
            v_effective_after := v_snapshot.revision;
            v_snapshot_payload := jsonb_build_object(
                'revision',v_snapshot.revision,
                'frontier_hash',v_snapshot.frontier_hash,
                'source_lineage_hash',v_snapshot.source_lineage_hash,
                'snapshot_hash',v_snapshot.snapshot_hash,
                'snapshot_size_bytes',v_snapshot.snapshot_size_bytes,
                'snapshot',v_snapshot.snapshot
            );
        ELSIF v_after_revision<v_snapshot.revision THEN
            RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_snapshot_cursor_conflict';
        END IF;
    END IF;

    WITH page AS (
        SELECT revision,parent_frontier_hash,delta_hash,next_frontier_hash,
               source_lineage_hash,source_run_id,delta
          FROM agent_store.session_memory_deltas
         WHERE tenant_id=v_run.tenant_id
           AND principal_id=v_run.principal_id
           AND session_id=v_run.session_id
           AND revision>v_effective_after
         ORDER BY revision
         LIMIT v_limit
    )
    SELECT COALESCE(jsonb_agg(jsonb_build_object(
               'revision',revision,
               'parent_frontier_hash',parent_frontier_hash,
               'delta_hash',delta_hash,
               'next_frontier_hash',next_frontier_hash,
               'source_lineage_hash',source_lineage_hash,
               'source_run_id',source_run_id,
               'delta',delta
           ) ORDER BY revision),'[]'::jsonb),
           COALESCE(max(revision),v_effective_after)
      INTO v_deltas,v_last_revision
      FROM page;
    SELECT EXISTS (
        SELECT 1 FROM agent_store.session_memory_deltas
         WHERE tenant_id=v_run.tenant_id
           AND principal_id=v_run.principal_id
           AND session_id=v_run.session_id
           AND revision>v_last_revision
    ) INTO v_has_more;
    RETURN jsonb_build_object(
        'run_id',v_run.run_id,
        'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,
        'memory_revision',v_memory.revision,
        'memory_frontier_hash',v_memory.frontier_hash,
        'memory_source_lineage_hash',v_memory.source_lineage_hash,
        'snapshot',v_snapshot_payload,
        'deltas',v_deltas,
        'next_after_revision',v_last_revision,
        'has_more',v_has_more
    );
END;
$$;

REVOKE ALL ON TABLE agent_store.session_memory_snapshots FROM PUBLIC;
REVOKE ALL ON TABLE agent_store.session_memory_snapshot_mutations FROM PUBLIC;
REVOKE ALL ON FUNCTION agent_store.reject_session_memory_snapshot_mutation() FROM PUBLIC;
REVOKE ALL ON FUNCTION agent_v1.checkpoint_session_memory_snapshot(jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION agent_v1.read_session_memory(jsonb) FROM PUBLIC;

COMMENT ON TABLE agent_store.session_memory_snapshots IS
    'Append-only canonical V3 projection checkpoints anchored to the full delta frontier.';
COMMENT ON FUNCTION agent_v1.checkpoint_session_memory_snapshot(jsonb) IS
    'Daemon-only owner/fence/revision/frontier CAS for a bounded typed memory snapshot.';

COMMIT;
