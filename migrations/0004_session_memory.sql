-- Append-only, owner-scoped session memory committed atomically with the
-- verified final answer. Runtime roles receive EXECUTE on agent_v1 procedures
-- only; they never receive direct table privileges.

BEGIN;

ALTER TABLE agent_store.runs
    ADD CONSTRAINT runs_session_memory_owner_v1_unique
    UNIQUE (run_id, tenant_id, principal_id, session_id);

CREATE TABLE agent_store.session_memory_frontiers (
    tenant_id text NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 128),
    principal_id text NOT NULL CHECK (char_length(principal_id) BETWEEN 1 AND 128),
    session_id text NOT NULL CHECK (char_length(session_id) BETWEEN 1 AND 128),
    revision bigint NOT NULL CHECK (revision >= 0),
    frontier_hash text NOT NULL CHECK (frontier_hash ~ '^sha256:[0-9a-f]{64}$'),
    source_lineage_hash text NOT NULL
        CHECK (source_lineage_hash ~ '^sha256:[0-9a-f]{64}$'),
    last_source_run_id text,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, principal_id, session_id),
    FOREIGN KEY (last_source_run_id, tenant_id, principal_id, session_id)
        REFERENCES agent_store.runs (run_id, tenant_id, principal_id, session_id)
        ON DELETE RESTRICT,
    CHECK ((revision = 0) = (last_source_run_id IS NULL))
);

CREATE TABLE agent_store.session_memory_deltas (
    tenant_id text NOT NULL,
    principal_id text NOT NULL,
    session_id text NOT NULL,
    revision bigint NOT NULL CHECK (revision >= 1),
    source_run_id text NOT NULL,
    parent_frontier_hash text NOT NULL
        CHECK (parent_frontier_hash ~ '^sha256:[0-9a-f]{64}$'),
    delta_hash text NOT NULL CHECK (delta_hash ~ '^sha256:[0-9a-f]{64}$'),
    next_frontier_hash text NOT NULL
        CHECK (next_frontier_hash ~ '^sha256:[0-9a-f]{64}$'),
    source_lineage_hash text NOT NULL
        CHECK (source_lineage_hash ~ '^sha256:[0-9a-f]{64}$'),
    delta jsonb NOT NULL CHECK (pg_column_size(delta) BETWEEN 2 AND 1048576),
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, principal_id, session_id, revision),
    UNIQUE (source_run_id),
    UNIQUE (tenant_id, principal_id, session_id, next_frontier_hash),
    FOREIGN KEY (tenant_id, principal_id, session_id)
        REFERENCES agent_store.session_memory_frontiers
        (tenant_id, principal_id, session_id) ON DELETE RESTRICT,
    FOREIGN KEY (source_run_id, tenant_id, principal_id, session_id)
        REFERENCES agent_store.runs (run_id, tenant_id, principal_id, session_id)
        ON DELETE RESTRICT,
    CHECK (jsonb_typeof(delta) = 'object'),
    CHECK (delta->>'schema_version' = '3'),
    CHECK (delta->>'authority' = 'context_only'),
    CHECK (delta->>'parent_frontier_hash' IS NOT DISTINCT FROM parent_frontier_hash),
    CHECK (delta->>'revision' IS NOT DISTINCT FROM revision::text),
    CHECK (delta->'source'->>'run_id' IS NOT DISTINCT FROM source_run_id),
    CHECK (delta->'source'->>'revision' IS NOT DISTINCT FROM revision::text),
    CHECK (delta->'recent_turn'->>'source_run_id' IS NOT DISTINCT FROM source_run_id)
);

CREATE INDEX session_memory_deltas_owner_page_idx
    ON agent_store.session_memory_deltas
    (tenant_id, principal_id, session_id, revision)
    INCLUDE (parent_frontier_hash, delta_hash, next_frontier_hash,
             source_lineage_hash, source_run_id);

CREATE FUNCTION agent_store.session_memory_frontier_hash(
    p_parent_hash text,
    p_revision bigint,
    p_delta_hash text
) RETURNS text
LANGUAGE plpgsql
IMMUTABLE
STRICT
SET search_path = pg_catalog
AS $$
BEGIN
    IF p_parent_hash !~ '^sha256:[0-9a-f]{64}$'
       OR p_delta_hash !~ '^sha256:[0-9a-f]{64}$'
       OR p_revision < 1 THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='invalid_memory_frontier_input';
    END IF;
    RETURN 'sha256:' || encode(
        sha256(
            convert_to('krw.session-memory/frontier-v2','UTF8')
            || decode('00','hex')
            || convert_to(p_parent_hash,'UTF8')
            || decode('00','hex')
            || convert_to(p_revision::text,'UTF8')
            || decode('00','hex')
            || convert_to(p_delta_hash,'UTF8')
        ),
        'hex'
    );
END;
$$;

CREATE FUNCTION agent_store.session_memory_source_lineage_hash(
    p_parent_hash text,
    p_revision bigint,
    p_source_run_hash text
) RETURNS text
LANGUAGE plpgsql
IMMUTABLE
STRICT
SET search_path = pg_catalog
AS $$
BEGIN
    IF p_parent_hash !~ '^sha256:[0-9a-f]{64}$'
       OR p_source_run_hash !~ '^sha256:[0-9a-f]{64}$'
       OR p_revision < 1 THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='invalid_memory_source_lineage_input';
    END IF;
    RETURN 'sha256:' || encode(
        sha256(
            convert_to('krw.session-memory/source-lineage-v2','UTF8')
            || decode('00','hex')
            || convert_to(p_parent_hash,'UTF8')
            || decode('00','hex')
            || convert_to(p_revision::text,'UTF8')
            || decode('00','hex')
            || convert_to(p_source_run_hash,'UTF8')
        ),
        'hex'
    );
END;
$$;

CREATE FUNCTION agent_store.reject_session_memory_delta_mutation() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $$
BEGIN
    RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_delta_is_append_only';
END;
$$;

CREATE TRIGGER session_memory_deltas_append_only
    BEFORE UPDATE OR DELETE ON agent_store.session_memory_deltas
    FOR EACH ROW EXECUTE FUNCTION agent_store.reject_session_memory_delta_mutation();

CREATE OR REPLACE FUNCTION agent_v1.commit_final(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_memory agent_store.session_memory_frontiers%ROWTYPE;
    v_existing_delta agent_store.session_memory_deltas%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
    v_entry record;
    v_has_memory boolean;
    v_existing_delta_found boolean := false;
    v_delta_revision bigint;
    v_parent_frontier_hash text;
    v_expected_next_frontier_hash text;
    v_source_run_hash text;
    v_expected_source_lineage_hash text;
    v_session_id_hash text;
    v_expected_final_commit_intent_hash text;
    v_expected_user_message_id text;
    v_expected_assistant_message_id text;
    v_memory_revision bigint;
    v_memory_frontier_hash text;
    v_memory_source_lineage_hash text;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','expected_cancel_generation','answer_bundle_hash',
              'final_output_hash','rendered_message_hash','answer_bundle','usage','settlement',
              'outbox_payloads','session_memory_delta_hash','session_memory_delta',
              'next_memory_frontier_hash'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','expected_cancel_generation','answer_bundle_hash',
              'final_output_hash','rendered_message_hash','answer_bundle','usage','settlement',
              'outbox_payloads']
    );
    IF NOT (p_request ? 'session_memory_delta_hash')
       OR NOT (p_request ? 'session_memory_delta')
       OR NOT (p_request ? 'next_memory_frontier_hash') THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='missing_session_memory_field';
    END IF;
    v_has_memory := p_request->'session_memory_delta' <> 'null'::jsonb;
    IF (p_request->'session_memory_delta_hash' <> 'null'::jsonb) IS DISTINCT FROM v_has_memory
       OR (p_request->'next_memory_frontier_hash' <> 'null'::jsonb) IS DISTINCT FROM v_has_memory THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='partial_session_memory_delta';
    END IF;
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'answer_bundle_hash');
    PERFORM agent_store.assert_hash(p_request->>'final_output_hash');
    PERFORM agent_store.assert_hash(p_request->>'rendered_message_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_json_size(p_request->'answer_bundle',8388608);
    PERFORM agent_store.assert_json_size(p_request->'usage',1048576);
    PERFORM agent_store.assert_json_size(p_request->'settlement',1048576);
    PERFORM agent_store.assert_json_size(p_request->'outbox_payloads',3145728);
    IF jsonb_typeof(p_request->'outbox_payloads') <> 'object' THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='outbox_payloads_must_be_object';
    END IF;
    IF EXISTS (
        SELECT 1 FROM jsonb_object_keys(p_request->'outbox_payloads') AS keys(key)
         WHERE keys.key NOT IN ('presentation','billing','notification')
    ) THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='unknown_outbox_kind';
    END IF;
    IF (p_request->'outbox_payloads')::text
       ~ '"(session_memory_delta|session_memory_delta_hash|next_memory_frontier_hash)"[[:space:]]*:' THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='session_memory_body_forbidden_in_outbox';
    END IF;
    IF v_has_memory THEN
        PERFORM agent_store.assert_hash(p_request->>'session_memory_delta_hash');
        PERFORM agent_store.assert_hash(p_request->>'next_memory_frontier_hash');
        PERFORM agent_store.assert_json_size(p_request->'session_memory_delta',1048576);
        IF jsonb_typeof(p_request->'session_memory_delta') <> 'object' THEN
            RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='session_memory_delta_must_be_object';
        END IF;
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
        v_run.run_id,p_request->>'mutation_id','commit_final',
        p_request->>'mutation_hash',p_request
    );
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state='cancelled' THEN
        RAISE EXCEPTION USING ERRCODE='K1010',MESSAGE='cancelled_before_final';
    END IF;
    IF v_run.state='final' THEN
        IF v_run.terminal_outcome->>'answer_bundle_hash' IS DISTINCT FROM p_request->>'answer_bundle_hash' THEN
            RAISE EXCEPTION USING ERRCODE='K1011',MESSAGE='final_conflict';
        END IF;
        SELECT * INTO v_existing_delta
          FROM agent_store.session_memory_deltas
         WHERE source_run_id=v_run.run_id;
        v_existing_delta_found := FOUND;
        IF v_has_memory IS DISTINCT FROM v_existing_delta_found THEN
            RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='final_memory_conflict';
        END IF;
        IF v_has_memory AND (
            v_existing_delta.delta_hash IS DISTINCT FROM p_request->>'session_memory_delta_hash'
            OR v_existing_delta.next_frontier_hash IS DISTINCT FROM p_request->>'next_memory_frontier_hash'
            OR v_existing_delta.delta IS DISTINCT FROM p_request->'session_memory_delta'
        ) THEN
            RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='final_memory_conflict';
        END IF;
        IF v_existing_delta_found THEN
            v_memory_revision := v_existing_delta.revision;
            v_memory_frontier_hash := v_existing_delta.next_frontier_hash;
            v_memory_source_lineage_hash := v_existing_delta.source_lineage_hash;
        END IF;
        v_response:=jsonb_build_object(
            'outcome','already_final','run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
            'run_version',v_run.run_version,'answer_bundle_hash',p_request->>'answer_bundle_hash',
            'memory_revision',v_memory_revision,
            'memory_frontier_hash',v_memory_frontier_hash,
            'memory_source_lineage_hash',v_memory_source_lineage_hash
        );
        PERFORM agent_store.record_mutation(
            v_run.run_id,p_request->>'mutation_id','commit_final',
            p_request->>'mutation_hash',p_request,v_response
        );
        RETURN v_response;
    END IF;
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
    IF EXISTS (
        SELECT 1 FROM agent_store.actions
         WHERE run_id=v_run.run_id AND stage IN ('begun','observed')
    ) THEN
        RAISE EXCEPTION USING ERRCODE='K1016',MESSAGE='pending_action';
    END IF;

    IF v_has_memory THEN
        v_delta_revision := agent_store.request_bigint(
            p_request->'session_memory_delta','revision',1,9223372036854775807
        );
        v_parent_frontier_hash := p_request->'session_memory_delta'->>'parent_frontier_hash';
        PERFORM agent_store.assert_hash(v_parent_frontier_hash);
        v_session_id_hash := 'sha256:' || encode(
            sha256(convert_to(v_run.session_id,'UTF8')),'hex'
        );
        v_expected_final_commit_intent_hash := 'sha256:' || encode(sha256(
            convert_to('krw.final-commit-intent/v2','UTF8')
            || decode('00','hex')
            || convert_to(v_run.run_id,'UTF8')
            || decode('00','hex')
            || convert_to(p_request->>'answer_bundle_hash','UTF8')
        ),'hex');
        v_expected_user_message_id := 'sha256:' || encode(sha256(
            convert_to('memory-message/v2','UTF8')
            || decode('00','hex')
            || convert_to(v_run.run_id,'UTF8')
            || decode('00','hex')
            || convert_to('user','UTF8')
        ),'hex');
        v_expected_assistant_message_id := 'sha256:' || encode(sha256(
            convert_to('memory-message/v2','UTF8')
            || decode('00','hex')
            || convert_to(v_run.run_id,'UTF8')
            || decode('00','hex')
            || convert_to('assistant','UTF8')
        ),'hex');
        IF p_request->'session_memory_delta'->>'schema_version' IS DISTINCT FROM '3'
           OR p_request->'session_memory_delta'->>'authority' IS DISTINCT FROM 'context_only'
           OR p_request->'session_memory_delta'->>'session_id_hash' IS DISTINCT FROM v_session_id_hash
           OR p_request->'session_memory_delta'->'source'->>'run_id' IS DISTINCT FROM v_run.run_id
           OR p_request->'session_memory_delta'->'source'->>'revision'
                IS DISTINCT FROM v_delta_revision::text
           OR p_request->'session_memory_delta'->'source'->>'user_message_id'
                IS DISTINCT FROM v_expected_user_message_id
           OR p_request->'session_memory_delta'->'source'->>'assistant_message_id'
                IS DISTINCT FROM v_expected_assistant_message_id
           OR p_request->'session_memory_delta'->'source'->>'final_commit_intent_hash'
                IS DISTINCT FROM v_expected_final_commit_intent_hash
           OR p_request->'session_memory_delta'->'source'->>'answer_bundle_hash'
                IS DISTINCT FROM p_request->>'answer_bundle_hash'
           OR p_request->'session_memory_delta'->'source'->>'final_output_hash'
                IS DISTINCT FROM p_request->>'final_output_hash'
           OR p_request->'session_memory_delta'->'recent_turn'->>'source_run_id'
                IS DISTINCT FROM v_run.run_id
           OR jsonb_typeof(p_request->'session_memory_delta'->'resolved_goals')
                IS DISTINCT FROM 'array' THEN
            RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_delta_identity_mismatch';
        END IF;

        INSERT INTO agent_store.session_memory_frontiers(
            tenant_id,principal_id,session_id,revision,frontier_hash,
            source_lineage_hash,last_source_run_id
        ) VALUES (
            v_run.tenant_id,v_run.principal_id,v_run.session_id,0,
            'sha256:a4e1b26a1a1e27b135aa73cb802316af633c59cc50e92358bca73d6c4e6a4fbb',
            'sha256:e0d62521a0b7c1b8d1f3ba5e090c6a7bd60261c57adae0f7ba496d4b06216d39',NULL
        ) ON CONFLICT (tenant_id,principal_id,session_id) DO NOTHING;
        SELECT * INTO v_memory
          FROM agent_store.session_memory_frontiers
         WHERE tenant_id=v_run.tenant_id
           AND principal_id=v_run.principal_id
           AND session_id=v_run.session_id
         FOR UPDATE;
        IF v_memory.frontier_hash IS DISTINCT FROM v_parent_frontier_hash
           OR v_memory.revision + 1 <> v_delta_revision THEN
            RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_frontier_conflict';
        END IF;
        v_expected_next_frontier_hash := agent_store.session_memory_frontier_hash(
            v_parent_frontier_hash,v_delta_revision,p_request->>'session_memory_delta_hash'
        );
        v_source_run_hash := 'sha256:' || encode(
            sha256(convert_to(v_run.run_id,'UTF8')),'hex'
        );
        v_expected_source_lineage_hash := agent_store.session_memory_source_lineage_hash(
            v_memory.source_lineage_hash,v_delta_revision,v_source_run_hash
        );
        IF v_expected_next_frontier_hash IS DISTINCT FROM p_request->>'next_memory_frontier_hash' THEN
            RAISE EXCEPTION USING ERRCODE='K1025',MESSAGE='session_memory_next_frontier_mismatch';
        END IF;
        INSERT INTO agent_store.session_memory_deltas(
            tenant_id,principal_id,session_id,revision,source_run_id,
            parent_frontier_hash,delta_hash,next_frontier_hash,source_lineage_hash,delta
        ) VALUES (
            v_run.tenant_id,v_run.principal_id,v_run.session_id,v_delta_revision,v_run.run_id,
            v_parent_frontier_hash,p_request->>'session_memory_delta_hash',
            v_expected_next_frontier_hash,v_expected_source_lineage_hash,
            p_request->'session_memory_delta'
        );
        UPDATE agent_store.session_memory_frontiers
           SET revision=v_delta_revision,
               frontier_hash=v_expected_next_frontier_hash,
               source_lineage_hash=v_expected_source_lineage_hash,
               last_source_run_id=v_run.run_id,
               updated_at=clock_timestamp()
         WHERE tenant_id=v_run.tenant_id
           AND principal_id=v_run.principal_id
           AND session_id=v_run.session_id;
        v_memory_revision := v_delta_revision;
        v_memory_frontier_hash := v_expected_next_frontier_hash;
        v_memory_source_lineage_hash := v_expected_source_lineage_hash;
    END IF;

    INSERT INTO agent_store.answer_bundles (
        run_id,answer_bundle_hash,final_output_hash,rendered_message_hash,answer_bundle,usage
    ) VALUES (
        v_run.run_id,p_request->>'answer_bundle_hash',p_request->>'final_output_hash',
        p_request->>'rendered_message_hash',p_request->'answer_bundle',p_request->'usage'
    );
    INSERT INTO agent_store.settlements (run_id,settlement_kind,settlement_payload)
    VALUES (v_run.run_id,'settled',p_request->'settlement');
    INSERT INTO agent_store.outbox (run_id,event_kind,dedupe_key,payload)
    VALUES (
        v_run.run_id,'answer.committed',v_run.run_id || ':answer.committed',
        jsonb_build_object(
            'run_id',v_run.run_id,'answer_bundle_hash',p_request->>'answer_bundle_hash',
            'rendered_message_hash',p_request->>'rendered_message_hash',
            'memory_revision',v_memory_revision,
            'memory_frontier_hash',v_memory_frontier_hash,
            'memory_source_lineage_hash',v_memory_source_lineage_hash
        )
    );
    FOR v_entry IN SELECT key,value FROM jsonb_each(p_request->'outbox_payloads') LOOP
        INSERT INTO agent_store.outbox (run_id,event_kind,dedupe_key,payload)
        VALUES (
            v_run.run_id,'answer.' || v_entry.key,
            v_run.run_id || ':answer.' || v_entry.key,v_entry.value
        );
    END LOOP;
    UPDATE agent_store.runs
       SET state='final',run_version=run_version+1,lease_owner=NULL,lease_deadline=NULL,
           terminal_outcome=jsonb_build_object(
               'kind','final','answer_bundle_hash',p_request->>'answer_bundle_hash'
           ),updated_at=clock_timestamp()
     WHERE run_id=v_run.run_id RETURNING * INTO v_run;
    v_response:=jsonb_build_object(
        'outcome','final_committed','run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'answer_bundle_hash',p_request->>'answer_bundle_hash',
        'memory_revision',v_memory_revision,
        'memory_frontier_hash',v_memory_frontier_hash,
        'memory_source_lineage_hash',v_memory_source_lineage_hash
    );
    PERFORM agent_store.record_mutation(
        v_run.run_id,p_request->>'mutation_id','commit_final',
        p_request->>'mutation_hash',p_request,v_response
    );
    RETURN v_response;
END;
$$;

CREATE FUNCTION agent_v1.read_session_memory(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_memory agent_store.session_memory_frontiers%ROWTYPE;
    v_after_revision bigint;
    v_limit bigint;
    v_deltas jsonb;
    v_last_revision bigint;
    v_has_more boolean;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','run_id','tenant_id','principal_id','session_id','fencing_token',
              'after_revision','limit'],
        ARRAY['abi_version','run_id','tenant_id','principal_id','session_id','fencing_token',
              'after_revision','limit']
    );
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'principal_id',128);
    PERFORM agent_store.assert_text(p_request,'session_id',128);
    v_after_revision := agent_store.request_bigint(
        p_request,'after_revision',0,9223372036854775807
    );
    v_limit := agent_store.request_bigint(p_request,'limit',1,8);

    SELECT * INTO v_run FROM agent_store.runs WHERE run_id=p_request->>'run_id';
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch';
    END IF;
    IF v_run.principal_id IS DISTINCT FROM p_request->>'principal_id'
       OR v_run.session_id IS DISTINCT FROM p_request->>'session_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1026',MESSAGE='session_memory_owner_mismatch';
    END IF;
    IF v_run.fencing_token<>agent_store.request_bigint(p_request,'fencing_token',0,9223372036854775807) THEN
        RAISE EXCEPTION USING ERRCODE='K1002',MESSAGE='stale_fence';
    END IF;
    IF v_run.state<>'active' THEN
        RAISE EXCEPTION USING ERRCODE='K1003',MESSAGE='terminal_or_inactive_run';
    END IF;
    IF v_run.lease_deadline<=clock_timestamp() THEN
        RAISE EXCEPTION USING ERRCODE='K1013',MESSAGE='lease_lost';
    END IF;

    SELECT * INTO v_memory
      FROM agent_store.session_memory_frontiers
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

    WITH page AS (
        SELECT revision,parent_frontier_hash,delta_hash,next_frontier_hash,
               source_lineage_hash,source_run_id,delta
          FROM agent_store.session_memory_deltas
         WHERE tenant_id=v_run.tenant_id
           AND principal_id=v_run.principal_id
           AND session_id=v_run.session_id
           AND revision>v_after_revision
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
           COALESCE(max(revision),v_after_revision)
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
        'deltas',v_deltas,
        'next_after_revision',v_last_revision,
        'has_more',v_has_more
    );
END;
$$;

REVOKE ALL ON TABLE agent_store.session_memory_frontiers FROM PUBLIC;
REVOKE ALL ON TABLE agent_store.session_memory_deltas FROM PUBLIC;
REVOKE ALL ON FUNCTION agent_store.session_memory_frontier_hash(text,bigint,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION agent_store.session_memory_source_lineage_hash(text,bigint,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION agent_store.reject_session_memory_delta_mutation() FROM PUBLIC;
REVOKE ALL ON FUNCTION agent_v1.read_session_memory(jsonb) FROM PUBLIC;

COMMENT ON TABLE agent_store.session_memory_frontiers IS
    'Tenant, principal, and session-owned CAS head for append-only context memory.';
COMMENT ON TABLE agent_store.session_memory_deltas IS
    'Append-only typed memory deltas; one source run advances one exact revision.';
COMMENT ON FUNCTION agent_v1.read_session_memory(jsonb) IS
    'Bounded EXECUTE-only memory read for the exact active claimed run owner and fence.';

COMMIT;
