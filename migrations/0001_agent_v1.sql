BEGIN;

CREATE SCHEMA IF NOT EXISTS agent_store;
CREATE SCHEMA IF NOT EXISTS agent_v1;

REVOKE ALL ON SCHEMA agent_store FROM PUBLIC;
REVOKE ALL ON SCHEMA agent_v1 FROM PUBLIC;

CREATE TABLE IF NOT EXISTS agent_store.session_claim_locks (
    tenant_id text NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 128),
    session_id text NOT NULL CHECK (char_length(session_id) BETWEEN 1 AND 128),
    PRIMARY KEY (tenant_id, session_id)
);

CREATE TABLE IF NOT EXISTS agent_store.fair_queue_clock (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    virtual_start_tag bigint NOT NULL DEFAULT 0 CHECK (virtual_start_tag >= 0)
);

INSERT INTO agent_store.fair_queue_clock(singleton, virtual_start_tag)
VALUES (true, 0)
ON CONFLICT (singleton) DO NOTHING;

CREATE TABLE IF NOT EXISTS agent_store.principal_fair_queue (
    tenant_id text NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 128),
    principal_id text NOT NULL CHECK (char_length(principal_id) BETWEEN 1 AND 128),
    tail_finish_tag bigint NOT NULL DEFAULT 0 CHECK (tail_finish_tag >= 0),
    PRIMARY KEY (tenant_id, principal_id)
);

CREATE TABLE IF NOT EXISTS agent_store.runs (
    run_id text PRIMARY KEY CHECK (char_length(run_id) BETWEEN 1 AND 128),
    tenant_id text NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 128),
    principal_id text NOT NULL CHECK (char_length(principal_id) BETWEEN 1 AND 128),
    session_id text NOT NULL CHECK (char_length(session_id) BETWEEN 1 AND 128),
    state text NOT NULL DEFAULT 'queued'
        CHECK (state IN ('queued', 'active', 'deferred', 'final', 'cancelled', 'failed')),
    fencing_token bigint NOT NULL DEFAULT 0 CHECK (fencing_token >= 0),
    run_version bigint NOT NULL DEFAULT 1 CHECK (run_version >= 1),
    cancel_generation bigint NOT NULL DEFAULT 0 CHECK (cancel_generation >= 0),
    checkpoint_seq bigint NOT NULL DEFAULT 0 CHECK (checkpoint_seq >= 0),
    action_frontier_seq bigint NOT NULL DEFAULT 0 CHECK (action_frontier_seq >= 0),
    action_frontier_hash text NOT NULL
        DEFAULT 'sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945'
        CHECK (action_frontier_hash ~ '^sha256:[0-9a-f]{64}$'),
    lease_owner text CHECK (lease_owner IS NULL OR char_length(lease_owner) BETWEEN 1 AND 128),
    lease_deadline timestamptz,
    agent_image_hash text NOT NULL CHECK (agent_image_hash ~ '^sha256:[0-9a-f]{64}$'),
    runtime_version text NOT NULL CHECK (char_length(runtime_version) BETWEEN 1 AND 64),
    priority smallint NOT NULL DEFAULT 0,
    immutable_snapshot_hash text NOT NULL
        CHECK (immutable_snapshot_hash ~ '^sha256:[0-9a-f]{64}$'),
    immutable_snapshot jsonb NOT NULL,
    resource_profile jsonb NOT NULL,
    budgets jsonb NOT NULL,
    terminal_outcome jsonb,
    queue_start_tag bigint NOT NULL DEFAULT 0 CHECK (queue_start_tag >= 0),
    queue_finish_tag bigint NOT NULL DEFAULT 1 CHECK (queue_finish_tag >= 1),
    available_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK ((lease_owner IS NULL) = (lease_deadline IS NULL)),
    CHECK ((state IN ('final', 'cancelled', 'failed')) = (terminal_outcome IS NOT NULL)),
    CHECK (queue_start_tag < 9223372036854775807),
    CHECK (queue_finish_tag = queue_start_tag + 1)
);

CREATE INDEX IF NOT EXISTS runs_expired_active_claim_idx
    ON agent_store.runs (lease_deadline, created_at, run_id)
    INCLUDE (tenant_id, session_id, runtime_version, agent_image_hash)
    WHERE state = 'active';

CREATE INDEX IF NOT EXISTS runs_fair_claim_order_idx
    ON agent_store.runs (
        queue_start_tag, queue_finish_tag, available_at, created_at, run_id
    )
    INCLUDE (tenant_id, principal_id, session_id, runtime_version, agent_image_hash)
    WHERE state IN ('queued', 'deferred');

CREATE INDEX IF NOT EXISTS runs_fair_claim_available_idx
    ON agent_store.runs (
        available_at, queue_start_tag, queue_finish_tag, created_at, run_id
    )
    INCLUDE (tenant_id, principal_id, session_id, runtime_version, agent_image_hash)
    WHERE state IN ('queued', 'deferred');

CREATE UNIQUE INDEX IF NOT EXISTS runs_one_active_per_session_idx
    ON agent_store.runs (tenant_id, session_id)
    WHERE state = 'active';

CREATE TABLE IF NOT EXISTS agent_store.mutations (
    run_id text NOT NULL REFERENCES agent_store.runs(run_id) ON DELETE RESTRICT,
    mutation_id text NOT NULL CHECK (char_length(mutation_id) BETWEEN 1 AND 128),
    operation text NOT NULL CHECK (char_length(operation) BETWEEN 1 AND 64),
    payload_hash text NOT NULL CHECK (payload_hash ~ '^sha256:[0-9a-f]{64}$'),
    request_payload jsonb NOT NULL,
    response_payload jsonb NOT NULL,
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (run_id, mutation_id)
);

CREATE TABLE IF NOT EXISTS agent_store.provider_episodes (
    run_id text NOT NULL REFERENCES agent_store.runs(run_id) ON DELETE RESTRICT,
    episode_hash text NOT NULL CHECK (episode_hash ~ '^sha256:[0-9a-f]{64}$'),
    checkpoint_seq bigint NOT NULL CHECK (checkpoint_seq >= 1),
    episode_artifact_ref text NOT NULL
        CHECK (char_length(episode_artifact_ref) BETWEEN 1 AND 2048),
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (run_id, episode_hash),
    UNIQUE (run_id, checkpoint_seq)
);

CREATE TABLE IF NOT EXISTS agent_store.actions (
    run_id text NOT NULL REFERENCES agent_store.runs(run_id) ON DELETE RESTRICT,
    action_key text NOT NULL CHECK (char_length(action_key) BETWEEN 1 AND 128),
    request_hash text NOT NULL CHECK (request_hash ~ '^sha256:[0-9a-f]{64}$'),
    episode_hash text NOT NULL,
    tool_call_id text NOT NULL CHECK (char_length(tool_call_id) BETWEEN 1 AND 128),
    capability_id text NOT NULL CHECK (char_length(capability_id) BETWEEN 1 AND 128),
    input_schema_hash text NOT NULL CHECK (input_schema_hash ~ '^sha256:[0-9a-f]{64}$'),
    output_schema_hash text NOT NULL CHECK (output_schema_hash ~ '^sha256:[0-9a-f]{64}$'),
    data_release_hash text NOT NULL CHECK (data_release_hash ~ '^sha256:[0-9a-f]{64}$'),
    arguments_artifact_ref text NOT NULL
        CHECK (char_length(arguments_artifact_ref) BETWEEN 1 AND 2048),
    retryable_read boolean NOT NULL,
    stage text NOT NULL CHECK (stage IN ('begun', 'observed', 'committed', 'ambiguous')),
    result_hash text CHECK (result_hash ~ '^sha256:[0-9a-f]{64}$'),
    result_artifact_ref text
        CHECK (result_artifact_ref IS NULL OR char_length(result_artifact_ref) BETWEEN 1 AND 2048),
    ambiguous_reason_code text
        CHECK (ambiguous_reason_code IS NULL OR char_length(ambiguous_reason_code) BETWEEN 1 AND 64),
    begun_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    observed_at timestamptz,
    committed_at timestamptz,
    PRIMARY KEY (run_id, action_key),
    FOREIGN KEY (run_id, episode_hash)
        REFERENCES agent_store.provider_episodes(run_id, episode_hash) ON DELETE RESTRICT,
    CHECK ((result_hash IS NULL) = (result_artifact_ref IS NULL)),
    CHECK ((stage IN ('observed', 'committed')) = (result_hash IS NOT NULL)),
    CHECK ((stage = 'ambiguous') = (ambiguous_reason_code IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS actions_recovery_idx
    ON agent_store.actions (run_id, stage)
    WHERE stage IN ('begun', 'observed', 'ambiguous');

CREATE TABLE IF NOT EXISTS agent_store.run_state_checkpoints (
    run_id text PRIMARY KEY REFERENCES agent_store.runs(run_id) ON DELETE RESTRICT,
    state_hash text NOT NULL CHECK (state_hash ~ '^sha256:[0-9a-f]{64}$'),
    state_artifact_ref text NOT NULL
        CHECK (char_length(state_artifact_ref) BETWEEN 1 AND 2048),
    state_size_bytes bigint NOT NULL CHECK (state_size_bytes BETWEEN 1 AND 8388608),
    recovery_schema_hash text NOT NULL
        CHECK (recovery_schema_hash ~ '^sha256:[0-9a-f]{64}$'),
    provider_checkpoint_seq bigint NOT NULL CHECK (provider_checkpoint_seq >= 0),
    action_frontier_seq bigint NOT NULL CHECK (action_frontier_seq >= 0),
    action_frontier_hash text NOT NULL
        CHECK (action_frontier_hash ~ '^sha256:[0-9a-f]{64}$'),
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE IF NOT EXISTS agent_store.answer_bundles (
    run_id text PRIMARY KEY REFERENCES agent_store.runs(run_id) ON DELETE RESTRICT,
    answer_bundle_hash text NOT NULL
        CHECK (answer_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
    final_output_hash text NOT NULL CHECK (final_output_hash ~ '^sha256:[0-9a-f]{64}$'),
    rendered_message_hash text NOT NULL
        CHECK (rendered_message_hash ~ '^sha256:[0-9a-f]{64}$'),
    answer_bundle jsonb NOT NULL,
    usage jsonb NOT NULL,
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE IF NOT EXISTS agent_store.settlements (
    run_id text PRIMARY KEY REFERENCES agent_store.runs(run_id) ON DELETE RESTRICT,
    settlement_kind text NOT NULL CHECK (settlement_kind IN ('settled', 'released')),
    settlement_payload jsonb NOT NULL,
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE IF NOT EXISTS agent_store.outbox (
    outbox_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    run_id text NOT NULL REFERENCES agent_store.runs(run_id) ON DELETE RESTRICT,
    event_kind text NOT NULL CHECK (char_length(event_kind) BETWEEN 1 AND 128),
    dedupe_key text NOT NULL CHECK (char_length(dedupe_key) BETWEEN 1 AND 256),
    payload jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    delivered_at timestamptz,
    delivery_owner text
        CHECK (delivery_owner IS NULL OR char_length(delivery_owner) BETWEEN 1 AND 128),
    delivery_deadline timestamptz,
    delivery_attempts integer NOT NULL DEFAULT 0 CHECK (delivery_attempts >= 0),
    last_error_hash text CHECK (last_error_hash ~ '^sha256:[0-9a-f]{64}$'),
    CHECK ((delivery_owner IS NULL) = (delivery_deadline IS NULL)),
    UNIQUE (event_kind, dedupe_key)
);

CREATE INDEX IF NOT EXISTS outbox_pending_idx
    ON agent_store.outbox (outbox_id)
    WHERE delivered_at IS NULL;

CREATE OR REPLACE FUNCTION agent_store.assert_request(
    p_request jsonb,
    p_allowed text[],
    p_required text[]
) RETURNS void
LANGUAGE plpgsql
IMMUTABLE
SET search_path = pg_catalog
AS $$
DECLARE
    v_key text;
    v_extra text;
BEGIN
    IF jsonb_typeof(p_request) IS DISTINCT FROM 'object' THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'request_must_be_object';
    END IF;
    IF p_request->>'abi_version' IS DISTINCT FROM '1' THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'unsupported_abi_version';
    END IF;
    SELECT string_agg(keys.key, ',' ORDER BY keys.key)
      INTO v_extra
      FROM jsonb_object_keys(p_request) AS keys(key)
     WHERE NOT (keys.key = ANY (p_allowed));
    IF v_extra IS NOT NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'unknown_request_fields';
    END IF;
    FOREACH v_key IN ARRAY p_required LOOP
        IF NOT (p_request ? v_key) OR p_request->v_key = 'null'::jsonb THEN
            RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'missing_request_field';
        END IF;
    END LOOP;
END;
$$;

CREATE OR REPLACE FUNCTION agent_store.assert_hash(p_value text) RETURNS void
LANGUAGE plpgsql
IMMUTABLE
SET search_path = pg_catalog
AS $$
BEGIN
    IF p_value IS NULL OR p_value !~ '^sha256:[0-9a-f]{64}$' THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'invalid_content_hash';
    END IF;
END;
$$;

CREATE OR REPLACE FUNCTION agent_store.advance_action_frontier(
    p_current_hash text,
    p_mutation_hash text
) RETURNS text
LANGUAGE plpgsql
IMMUTABLE
SET search_path = pg_catalog
AS $$
BEGIN
    IF p_current_hash IS NULL OR p_current_hash !~ '^sha256:[0-9a-f]{64}$'
       OR p_mutation_hash IS NULL OR p_mutation_hash !~ '^sha256:[0-9a-f]{64}$' THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='invalid_action_frontier_input';
    END IF;
    RETURN 'sha256:' || encode(
        sha256(convert_to(
            'krw-agent-action-frontier/v1|' || p_current_hash || '|' || p_mutation_hash,
            'UTF8'
        )),
        'hex'
    );
END;
$$;

CREATE OR REPLACE FUNCTION agent_store.request_bigint(
    p_request jsonb,
    p_key text,
    p_min bigint,
    p_max bigint
) RETURNS bigint
LANGUAGE plpgsql
IMMUTABLE
SET search_path = pg_catalog
AS $$
DECLARE
    v_value bigint;
BEGIN
    IF jsonb_typeof(p_request->p_key) <> 'number' THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'integer_field_required';
    END IF;
    BEGIN
        v_value := (p_request->>p_key)::bigint;
    EXCEPTION
        WHEN invalid_text_representation OR numeric_value_out_of_range THEN
            RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'integer_field_out_of_range';
    END;
    IF v_value < p_min OR v_value > p_max THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'integer_field_out_of_range';
    END IF;
    RETURN v_value;
END;
$$;

CREATE OR REPLACE FUNCTION agent_store.request_boolean(p_request jsonb, p_key text) RETURNS boolean
LANGUAGE plpgsql
IMMUTABLE
SET search_path = pg_catalog
AS $$
BEGIN
    IF jsonb_typeof(p_request->p_key) <> 'boolean' THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'boolean_field_required';
    END IF;
    RETURN (p_request->>p_key)::boolean;
END;
$$;

CREATE OR REPLACE FUNCTION agent_store.assert_text(
    p_request jsonb,
    p_key text,
    p_max_length integer
) RETURNS void
LANGUAGE plpgsql
IMMUTABLE
SET search_path = pg_catalog
AS $$
BEGIN
    IF jsonb_typeof(p_request->p_key) <> 'string'
       OR char_length(p_request->>p_key) NOT BETWEEN 1 AND p_max_length THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'invalid_text_field';
    END IF;
END;
$$;

CREATE OR REPLACE FUNCTION agent_store.assert_json_size(
    p_value jsonb,
    p_max_bytes integer
) RETURNS void
LANGUAGE plpgsql
IMMUTABLE
SET search_path = pg_catalog
AS $$
BEGIN
    IF p_value IS NULL OR pg_column_size(p_value) > p_max_bytes THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'json_payload_too_large';
    END IF;
END;
$$;

CREATE OR REPLACE FUNCTION agent_store.allocate_fair_queue_tag(
    p_tenant_id text,
    p_principal_id text
) RETURNS TABLE(queue_start_tag bigint, queue_finish_tag bigint)
LANGUAGE plpgsql
VOLATILE
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_virtual_start_tag bigint;
    v_tail_finish_tag bigint;
BEGIN
    IF p_tenant_id IS NULL OR char_length(p_tenant_id) NOT BETWEEN 1 AND 128
       OR p_principal_id IS NULL OR char_length(p_principal_id) NOT BETWEEN 1 AND 128 THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='invalid_fair_queue_principal';
    END IF;

    SELECT clock.virtual_start_tag
      INTO v_virtual_start_tag
      FROM agent_store.fair_queue_clock AS clock
     WHERE clock.singleton
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='fair_queue_clock_missing';
    END IF;

    INSERT INTO agent_store.principal_fair_queue(
        tenant_id, principal_id, tail_finish_tag
    ) VALUES (
        p_tenant_id, p_principal_id, 0
    ) ON CONFLICT (tenant_id, principal_id) DO NOTHING;

    SELECT principal.tail_finish_tag
      INTO v_tail_finish_tag
      FROM agent_store.principal_fair_queue AS principal
     WHERE principal.tenant_id = p_tenant_id
       AND principal.principal_id = p_principal_id
     FOR UPDATE;

    queue_start_tag := GREATEST(v_virtual_start_tag, v_tail_finish_tag);
    IF queue_start_tag >= 9223372036854775807 THEN
        RAISE EXCEPTION USING ERRCODE='K1022',MESSAGE='fair_queue_tag_overflow';
    END IF;
    queue_finish_tag := queue_start_tag + 1;

    UPDATE agent_store.principal_fair_queue
       SET tail_finish_tag = queue_finish_tag
     WHERE tenant_id = p_tenant_id
       AND principal_id = p_principal_id;
    RETURN NEXT;
END;
$$;

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
       OR v_stored.payload_hash IS DISTINCT FROM p_payload_hash
       OR v_stored.request_payload IS DISTINCT FROM p_request_payload THEN
        RAISE EXCEPTION USING ERRCODE = 'K1004', MESSAGE = 'mutation_conflict';
    END IF;
    RETURN v_stored.response_payload;
END;
$$;

CREATE OR REPLACE FUNCTION agent_store.record_mutation(
    p_run_id text,
    p_mutation_id text,
    p_operation text,
    p_payload_hash text,
    p_request_payload jsonb,
    p_response_payload jsonb
) RETURNS void
LANGUAGE sql
VOLATILE
SET search_path = pg_catalog, agent_store
AS $$
    INSERT INTO agent_store.mutations (
        run_id, mutation_id, operation, payload_hash, request_payload, response_payload
    ) VALUES (
        p_run_id, p_mutation_id, p_operation, p_payload_hash,
        p_request_payload, p_response_payload
    );
$$;

CREATE OR REPLACE FUNCTION agent_v1.enqueue_run(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_response jsonb;
    v_replay jsonb;
    v_inserted integer;
    v_queue_start_tag bigint;
    v_queue_finish_tag bigint;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','principal_id','session_id',
              'agent_image_hash','runtime_version','priority','immutable_snapshot_hash',
              'immutable_snapshot','resource_profile','budgets'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','principal_id','session_id',
              'agent_image_hash','runtime_version','priority','immutable_snapshot_hash',
              'immutable_snapshot','resource_profile','budgets']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'agent_image_hash');
    PERFORM agent_store.assert_hash(p_request->>'immutable_snapshot_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'principal_id',128);
    PERFORM agent_store.assert_text(p_request,'session_id',128);
    PERFORM agent_store.assert_text(p_request,'runtime_version',64);
    PERFORM agent_store.assert_json_size(p_request->'immutable_snapshot',1048576);
    PERFORM agent_store.assert_json_size(p_request->'resource_profile',65536);
    PERFORM agent_store.assert_json_size(p_request->'budgets',65536);

    INSERT INTO agent_store.runs (
        run_id, tenant_id, principal_id, session_id, agent_image_hash, runtime_version, priority,
        immutable_snapshot_hash, immutable_snapshot, resource_profile, budgets
    ) VALUES (
        p_request->>'run_id', p_request->>'tenant_id', p_request->>'principal_id',
        p_request->>'session_id',
        p_request->>'agent_image_hash', p_request->>'runtime_version',
        agent_store.request_bigint(p_request,'priority',-32768,32767)::smallint,
        p_request->>'immutable_snapshot_hash',
        p_request->'immutable_snapshot', p_request->'resource_profile', p_request->'budgets'
    ) ON CONFLICT (run_id) DO NOTHING;
    GET DIAGNOSTICS v_inserted = ROW_COUNT;

    IF v_inserted = 1 THEN
        SELECT allocated.queue_start_tag, allocated.queue_finish_tag
          INTO v_queue_start_tag, v_queue_finish_tag
          FROM agent_store.allocate_fair_queue_tag(
              p_request->>'tenant_id', p_request->>'principal_id'
          ) AS allocated;
        UPDATE agent_store.runs
           SET queue_start_tag = v_queue_start_tag,
               queue_finish_tag = v_queue_finish_tag
         WHERE run_id = p_request->>'run_id';
        INSERT INTO agent_store.session_claim_locks(tenant_id,session_id)
        VALUES(p_request->>'tenant_id',p_request->>'session_id')
        ON CONFLICT (tenant_id,session_id) DO NOTHING;
    END IF;

    SELECT * INTO v_run FROM agent_store.runs
     WHERE run_id = p_request->>'run_id' FOR UPDATE;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id'
       OR v_run.principal_id IS DISTINCT FROM p_request->>'principal_id'
       OR v_run.session_id IS DISTINCT FROM p_request->>'session_id'
       OR v_run.agent_image_hash IS DISTINCT FROM p_request->>'agent_image_hash'
       OR v_run.runtime_version IS DISTINCT FROM p_request->>'runtime_version'
       OR v_run.immutable_snapshot_hash IS DISTINCT FROM p_request->>'immutable_snapshot_hash'
       OR v_run.immutable_snapshot IS DISTINCT FROM p_request->'immutable_snapshot'
       OR v_run.resource_profile IS DISTINCT FROM p_request->'resource_profile'
       OR v_run.budgets IS DISTINCT FROM p_request->'budgets' THEN
        RAISE EXCEPTION USING ERRCODE = 'K1004', MESSAGE = 'run_identity_conflict';
    END IF;

    v_replay := agent_store.replay_mutation(
        v_run.run_id, p_request->>'mutation_id', 'enqueue_run',
        p_request->>'mutation_hash', p_request
    );
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;

    v_response := jsonb_build_object(
        'outcome', CASE WHEN v_inserted = 1 THEN 'enqueued' ELSE 'already_enqueued' END,
        'run_id', v_run.run_id,
        'run_version', v_run.run_version
    );
    PERFORM agent_store.record_mutation(
        v_run.run_id, p_request->>'mutation_id', 'enqueue_run',
        p_request->>'mutation_hash', p_request, v_response
    );
    RETURN v_response;
END;
$$;

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
                        'ambiguous_reason_code',ambiguous_reason_code
                    ) ORDER BY begun_at, action_key)
                      FROM agent_store.actions
                     WHERE run_id=v_run.run_id
                ), '[]'::jsonb)
            )
        )
    );
END;
$$;

CREATE OR REPLACE FUNCTION agent_v1.renew_lease(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
    v_lease_ms bigint;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','worker_id','lease_ms'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','worker_id','lease_ms']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'worker_id',128);
    v_lease_ms := agent_store.request_bigint(p_request,'lease_ms',1000,600000);
    IF v_lease_ms < 1000 OR v_lease_ms > 600000 THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'invalid_lease_ms';
    END IF;

    SELECT * INTO v_run FROM agent_store.runs
     WHERE run_id = p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = 'K1001', MESSAGE = 'unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN
        RAISE EXCEPTION USING ERRCODE = 'K1017', MESSAGE = 'tenant_mismatch';
    END IF;
    IF v_run.fencing_token <> agent_store.request_bigint(p_request,'fencing_token',0,9223372036854775807) THEN
        RAISE EXCEPTION USING ERRCODE = 'K1002', MESSAGE = 'stale_fence';
    END IF;
    v_replay := agent_store.replay_mutation(
        v_run.run_id, p_request->>'mutation_id', 'renew_lease',
        p_request->>'mutation_hash', p_request
    );
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state <> 'active' OR v_run.lease_owner IS DISTINCT FROM p_request->>'worker_id'
       OR v_run.lease_deadline <= clock_timestamp() THEN
        RAISE EXCEPTION USING ERRCODE = 'K1013', MESSAGE = 'lease_lost';
    END IF;
    IF v_run.run_version <> agent_store.request_bigint(p_request,'expected_run_version',1,9223372036854775807) THEN
        RAISE EXCEPTION USING ERRCODE = 'K1012', MESSAGE = 'run_version_mismatch';
    END IF;

    UPDATE agent_store.runs
       SET run_version = run_version + 1,
           lease_deadline = clock_timestamp() + v_lease_ms * interval '1 millisecond',
           updated_at = clock_timestamp()
     WHERE run_id = v_run.run_id RETURNING * INTO v_run;
    v_response := jsonb_build_object(
        'outcome','renewed','run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'cancel_generation',v_run.cancel_generation,
        'checkpoint_seq',v_run.checkpoint_seq
    );
    PERFORM agent_store.record_mutation(
        v_run.run_id,p_request->>'mutation_id','renew_lease',
        p_request->>'mutation_hash',p_request,v_response
    );
    RETURN v_response;
END;
$$;

CREATE OR REPLACE FUNCTION agent_v1.checkpoint_episode(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_existing agent_store.provider_episodes%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','expected_checkpoint_seq','episode_hash','episode_artifact_ref'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','expected_checkpoint_seq','episode_hash','episode_artifact_ref']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'episode_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'episode_artifact_ref',2048);
    SELECT * INTO v_run FROM agent_store.runs
     WHERE run_id = p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = 'K1001', MESSAGE = 'unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN
        RAISE EXCEPTION USING ERRCODE = 'K1017', MESSAGE = 'tenant_mismatch';
    END IF;
    IF v_run.fencing_token <> agent_store.request_bigint(p_request,'fencing_token',0,9223372036854775807) THEN
        RAISE EXCEPTION USING ERRCODE = 'K1002', MESSAGE = 'stale_fence';
    END IF;
    v_replay := agent_store.replay_mutation(
        v_run.run_id,p_request->>'mutation_id','checkpoint_episode',
        p_request->>'mutation_hash',p_request
    );
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state <> 'active' THEN RAISE EXCEPTION USING ERRCODE = 'K1003', MESSAGE = 'terminal_or_inactive_run'; END IF;
    IF v_run.lease_deadline <= clock_timestamp() THEN RAISE EXCEPTION USING ERRCODE = 'K1013', MESSAGE = 'lease_lost'; END IF;
    IF v_run.run_version <> agent_store.request_bigint(p_request,'expected_run_version',1,9223372036854775807) THEN
        RAISE EXCEPTION USING ERRCODE = 'K1012', MESSAGE = 'run_version_mismatch';
    END IF;

    SELECT * INTO v_existing FROM agent_store.provider_episodes
     WHERE run_id = v_run.run_id AND episode_hash = p_request->>'episode_hash';
    IF FOUND THEN
        IF v_existing.episode_artifact_ref IS DISTINCT FROM p_request->>'episode_artifact_ref' THEN
            RAISE EXCEPTION USING ERRCODE = 'K1004', MESSAGE = 'episode_identity_conflict';
        END IF;
        v_response := jsonb_build_object(
            'outcome','already_checkpointed','run_id',v_run.run_id,
            'fencing_token',v_run.fencing_token,'run_version',v_run.run_version,
            'cancel_generation',v_run.cancel_generation,'checkpoint_seq',v_run.checkpoint_seq
        );
        PERFORM agent_store.record_mutation(
            v_run.run_id,p_request->>'mutation_id','checkpoint_episode',
            p_request->>'mutation_hash',p_request,v_response
        );
        RETURN v_response;
    END IF;
    IF v_run.checkpoint_seq <> agent_store.request_bigint(p_request,'expected_checkpoint_seq',0,9223372036854775807) THEN
        RAISE EXCEPTION USING ERRCODE = 'K1015', MESSAGE = 'checkpoint_sequence_mismatch';
    END IF;
    IF (SELECT count(*) FROM agent_store.provider_episodes WHERE run_id=v_run.run_id) >= 4096 THEN
        RAISE EXCEPTION USING ERRCODE='K1018',MESSAGE='recovery_receipt_limit_exceeded';
    END IF;

    INSERT INTO agent_store.provider_episodes (
        run_id,episode_hash,checkpoint_seq,episode_artifact_ref
    ) VALUES (
        v_run.run_id,p_request->>'episode_hash',v_run.checkpoint_seq + 1,
        p_request->>'episode_artifact_ref'
    );
    UPDATE agent_store.runs
       SET checkpoint_seq = checkpoint_seq + 1, run_version = run_version + 1,
           updated_at = clock_timestamp()
     WHERE run_id = v_run.run_id RETURNING * INTO v_run;
    v_response := jsonb_build_object(
        'outcome','checkpointed','run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'cancel_generation',v_run.cancel_generation,
        'checkpoint_seq',v_run.checkpoint_seq
    );
    PERFORM agent_store.record_mutation(
        v_run.run_id,p_request->>'mutation_id','checkpoint_episode',
        p_request->>'mutation_hash',p_request,v_response
    );
    RETURN v_response;
END;
$$;

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
              'state_size_bytes'],
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

CREATE OR REPLACE FUNCTION agent_v1.begin_action(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_action agent_store.actions%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','action_key','request_hash','episode_hash','tool_call_id',
              'capability_id','input_schema_hash','output_schema_hash','data_release_hash',
              'arguments_artifact_ref','retryable_read'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','action_key','request_hash','episode_hash','tool_call_id',
              'capability_id','input_schema_hash','output_schema_hash','data_release_hash',
              'arguments_artifact_ref','retryable_read']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'request_hash');
    PERFORM agent_store.assert_hash(p_request->>'episode_hash');
    PERFORM agent_store.assert_hash(p_request->>'input_schema_hash');
    PERFORM agent_store.assert_hash(p_request->>'output_schema_hash');
    PERFORM agent_store.assert_hash(p_request->>'data_release_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'action_key',128);
    PERFORM agent_store.assert_text(p_request,'tool_call_id',128);
    PERFORM agent_store.assert_text(p_request,'capability_id',128);
    PERFORM agent_store.assert_text(p_request,'arguments_artifact_ref',2048);
    SELECT * INTO v_run FROM agent_store.runs
     WHERE run_id = p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = 'K1001', MESSAGE = 'unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN RAISE EXCEPTION USING ERRCODE = 'K1017', MESSAGE = 'tenant_mismatch'; END IF;
    IF v_run.fencing_token <> agent_store.request_bigint(p_request,'fencing_token',0,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE = 'K1002', MESSAGE = 'stale_fence'; END IF;
    v_replay := agent_store.replay_mutation(v_run.run_id,p_request->>'mutation_id','begin_action',p_request->>'mutation_hash',p_request);
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state <> 'active' THEN RAISE EXCEPTION USING ERRCODE = 'K1003', MESSAGE = 'terminal_or_inactive_run'; END IF;
    IF v_run.lease_deadline <= clock_timestamp() THEN RAISE EXCEPTION USING ERRCODE = 'K1013', MESSAGE = 'lease_lost'; END IF;
    IF v_run.run_version <> agent_store.request_bigint(p_request,'expected_run_version',1,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE = 'K1012', MESSAGE = 'run_version_mismatch'; END IF;
    IF NOT EXISTS (
        SELECT 1 FROM agent_store.provider_episodes
         WHERE run_id = v_run.run_id AND episode_hash = p_request->>'episode_hash'
    ) THEN
        RAISE EXCEPTION USING ERRCODE = 'K1014', MESSAGE = 'episode_not_committed';
    END IF;

    SELECT * INTO v_action FROM agent_store.actions
     WHERE run_id = v_run.run_id AND action_key = p_request->>'action_key';
    IF FOUND THEN
        IF v_action.request_hash IS DISTINCT FROM p_request->>'request_hash'
           OR v_action.episode_hash IS DISTINCT FROM p_request->>'episode_hash'
           OR v_action.tool_call_id IS DISTINCT FROM p_request->>'tool_call_id'
           OR v_action.capability_id IS DISTINCT FROM p_request->>'capability_id'
           OR v_action.input_schema_hash IS DISTINCT FROM p_request->>'input_schema_hash'
           OR v_action.output_schema_hash IS DISTINCT FROM p_request->>'output_schema_hash'
           OR v_action.data_release_hash IS DISTINCT FROM p_request->>'data_release_hash'
           OR v_action.arguments_artifact_ref IS DISTINCT FROM p_request->>'arguments_artifact_ref'
           OR v_action.retryable_read IS DISTINCT FROM agent_store.request_boolean(p_request,'retryable_read') THEN
            RAISE EXCEPTION USING ERRCODE = 'K1005', MESSAGE = 'action_identity_conflict';
        END IF;
        v_response := jsonb_build_object(
            'outcome','already_begun','run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
            'run_version',v_run.run_version,'action_key',v_action.action_key,'stage',v_action.stage,
            'request_hash',v_action.request_hash,'result_hash',v_action.result_hash,
            'retryable_read',v_action.retryable_read,
            'action_frontier_seq',v_run.action_frontier_seq,
            'action_frontier_hash',v_run.action_frontier_hash
        );
        PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','begin_action',p_request->>'mutation_hash',p_request,v_response);
        RETURN v_response;
    END IF;
    IF (
        SELECT count(*) FROM agent_store.actions
         WHERE run_id=v_run.run_id AND stage IN ('begun','observed','ambiguous')
    ) >= 128 THEN
        RAISE EXCEPTION USING ERRCODE='K1018',MESSAGE='recovery_receipt_limit_exceeded';
    END IF;
    IF (SELECT count(*) FROM agent_store.actions WHERE run_id=v_run.run_id) >= 4096 THEN
        RAISE EXCEPTION USING ERRCODE='K1018',MESSAGE='action_receipt_limit_exceeded';
    END IF;

    INSERT INTO agent_store.actions (
        run_id,action_key,request_hash,episode_hash,tool_call_id,capability_id,
        input_schema_hash,output_schema_hash,data_release_hash,arguments_artifact_ref,
        retryable_read,stage
    ) VALUES (
        v_run.run_id,p_request->>'action_key',p_request->>'request_hash',
        p_request->>'episode_hash',p_request->>'tool_call_id',p_request->>'capability_id',
        p_request->>'input_schema_hash',p_request->>'output_schema_hash',
        p_request->>'data_release_hash',p_request->>'arguments_artifact_ref',
        agent_store.request_boolean(p_request,'retryable_read'),'begun'
    ) RETURNING * INTO v_action;
    UPDATE agent_store.runs
       SET run_version=run_version+1,action_frontier_seq=action_frontier_seq+1,
           action_frontier_hash=agent_store.advance_action_frontier(
               action_frontier_hash,p_request->>'mutation_hash'
           ),updated_at=clock_timestamp()
     WHERE run_id = v_run.run_id RETURNING * INTO v_run;
    v_response := jsonb_build_object(
        'outcome','begun','run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'action_key',v_action.action_key,'stage',v_action.stage,
        'request_hash',v_action.request_hash,'result_hash',v_action.result_hash,
        'retryable_read',v_action.retryable_read,
        'action_frontier_seq',v_run.action_frontier_seq,
        'action_frontier_hash',v_run.action_frontier_hash
    );
    PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','begin_action',p_request->>'mutation_hash',p_request,v_response);
    RETURN v_response;
END;
$$;

CREATE OR REPLACE FUNCTION agent_v1.observe_action(p_request jsonb) RETURNS jsonb
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
              'expected_run_version','action_key','result_hash','result_artifact_ref'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','action_key','result_hash','result_artifact_ref']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'result_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'action_key',128);
    PERFORM agent_store.assert_text(p_request,'result_artifact_ref',2048);
    SELECT * INTO v_run FROM agent_store.runs WHERE run_id = p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = 'K1001', MESSAGE = 'unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN RAISE EXCEPTION USING ERRCODE = 'K1017', MESSAGE = 'tenant_mismatch'; END IF;
    IF v_run.fencing_token <> agent_store.request_bigint(p_request,'fencing_token',0,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE = 'K1002', MESSAGE = 'stale_fence'; END IF;
    v_replay := agent_store.replay_mutation(v_run.run_id,p_request->>'mutation_id','observe_action',p_request->>'mutation_hash',p_request);
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state <> 'active' THEN RAISE EXCEPTION USING ERRCODE = 'K1003', MESSAGE = 'terminal_or_inactive_run'; END IF;
    IF v_run.lease_deadline <= clock_timestamp() THEN RAISE EXCEPTION USING ERRCODE = 'K1013', MESSAGE = 'lease_lost'; END IF;
    IF v_run.run_version <> agent_store.request_bigint(p_request,'expected_run_version',1,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE = 'K1012', MESSAGE = 'run_version_mismatch'; END IF;
    SELECT * INTO v_action FROM agent_store.actions
     WHERE run_id = v_run.run_id AND action_key = p_request->>'action_key' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = 'K1006', MESSAGE = 'unknown_action'; END IF;
    IF v_action.stage = 'ambiguous' THEN RAISE EXCEPTION USING ERRCODE = 'K1007', MESSAGE = 'ambiguous_action_cannot_be_observed'; END IF;
    IF v_action.result_hash IS NOT NULL THEN
        IF v_action.result_hash IS DISTINCT FROM p_request->>'result_hash'
           OR v_action.result_artifact_ref IS DISTINCT FROM p_request->>'result_artifact_ref' THEN
            RAISE EXCEPTION USING ERRCODE = 'K1007', MESSAGE = 'observation_conflict';
        END IF;
        v_outcome := 'already_observed';
    ELSE
        UPDATE agent_store.actions
           SET stage='observed',result_hash=p_request->>'result_hash',
               result_artifact_ref=p_request->>'result_artifact_ref',observed_at=clock_timestamp()
         WHERE run_id=v_run.run_id AND action_key=v_action.action_key RETURNING * INTO v_action;
        UPDATE agent_store.runs
           SET run_version=run_version+1,action_frontier_seq=action_frontier_seq+1,
               action_frontier_hash=agent_store.advance_action_frontier(
                   action_frontier_hash,p_request->>'mutation_hash'
               ),updated_at=clock_timestamp()
         WHERE run_id=v_run.run_id RETURNING * INTO v_run;
        v_outcome := 'observed';
    END IF;
    v_response := jsonb_build_object(
        'outcome',v_outcome,'run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'action_key',v_action.action_key,'stage',v_action.stage,
        'request_hash',v_action.request_hash,'result_hash',v_action.result_hash,
        'retryable_read',v_action.retryable_read,
        'action_frontier_seq',v_run.action_frontier_seq,
        'action_frontier_hash',v_run.action_frontier_hash
    );
    PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','observe_action',p_request->>'mutation_hash',p_request,v_response);
    RETURN v_response;
END;
$$;

CREATE OR REPLACE FUNCTION agent_v1.commit_action(p_request jsonb) RETURNS jsonb
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
              'expected_run_version','action_key','result_hash'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','action_key','result_hash']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_hash(p_request->>'result_hash');
    SELECT * INTO v_run FROM agent_store.runs WHERE run_id=p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch'; END IF;
    IF v_run.fencing_token<>agent_store.request_bigint(p_request,'fencing_token',0,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE='K1002',MESSAGE='stale_fence'; END IF;
    v_replay:=agent_store.replay_mutation(v_run.run_id,p_request->>'mutation_id','commit_action',p_request->>'mutation_hash',p_request);
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state<>'active' THEN RAISE EXCEPTION USING ERRCODE='K1003',MESSAGE='terminal_or_inactive_run'; END IF;
    IF v_run.lease_deadline<=clock_timestamp() THEN RAISE EXCEPTION USING ERRCODE='K1013',MESSAGE='lease_lost'; END IF;
    IF v_run.run_version<>agent_store.request_bigint(p_request,'expected_run_version',1,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE='K1012',MESSAGE='run_version_mismatch'; END IF;
    SELECT * INTO v_action FROM agent_store.actions
     WHERE run_id=v_run.run_id AND action_key=p_request->>'action_key' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1006',MESSAGE='unknown_action'; END IF;
    IF v_action.result_hash IS DISTINCT FROM p_request->>'result_hash' THEN
        RAISE EXCEPTION USING ERRCODE='K1008',MESSAGE='commit_without_matching_observation';
    END IF;
    IF v_action.stage='committed' THEN
        v_outcome:='already_committed';
    ELSIF v_action.stage='observed' THEN
        UPDATE agent_store.actions SET stage='committed',committed_at=clock_timestamp()
         WHERE run_id=v_run.run_id AND action_key=v_action.action_key RETURNING * INTO v_action;
        UPDATE agent_store.runs
           SET run_version=run_version+1,action_frontier_seq=action_frontier_seq+1,
               action_frontier_hash=agent_store.advance_action_frontier(
                   action_frontier_hash,p_request->>'mutation_hash'
               ),updated_at=clock_timestamp()
         WHERE run_id=v_run.run_id RETURNING * INTO v_run;
        v_outcome:='committed';
    ELSE
        RAISE EXCEPTION USING ERRCODE='K1008',MESSAGE='commit_without_matching_observation';
    END IF;
    v_response:=jsonb_build_object(
        'outcome',v_outcome,'run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'action_key',v_action.action_key,'stage',v_action.stage,
        'request_hash',v_action.request_hash,'result_hash',v_action.result_hash,
        'retryable_read',v_action.retryable_read,
        'action_frontier_seq',v_run.action_frontier_seq,
        'action_frontier_hash',v_run.action_frontier_hash
    );
    PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','commit_action',p_request->>'mutation_hash',p_request,v_response);
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
    IF v_action.stage='committed' THEN
        v_outcome:='already_committed';
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
        'action_frontier_seq',v_run.action_frontier_seq,
        'action_frontier_hash',v_run.action_frontier_hash
    );
    PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','mark_action_ambiguous',p_request->>'mutation_hash',p_request,v_response);
    RETURN v_response;
END;
$$;

CREATE OR REPLACE FUNCTION agent_v1.commit_final(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
    v_entry record;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','expected_cancel_generation','answer_bundle_hash',
              'final_output_hash','rendered_message_hash','answer_bundle','usage','settlement',
              'outbox_payloads'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','fencing_token',
              'expected_run_version','expected_cancel_generation','answer_bundle_hash',
              'final_output_hash','rendered_message_hash','answer_bundle','usage','settlement',
              'outbox_payloads']
    );
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

    SELECT * INTO v_run FROM agent_store.runs WHERE run_id=p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch'; END IF;
    IF v_run.fencing_token<>agent_store.request_bigint(p_request,'fencing_token',0,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE='K1002',MESSAGE='stale_fence'; END IF;
    v_replay:=agent_store.replay_mutation(v_run.run_id,p_request->>'mutation_id','commit_final',p_request->>'mutation_hash',p_request);
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;
    IF v_run.state='cancelled' THEN RAISE EXCEPTION USING ERRCODE='K1010',MESSAGE='cancelled_before_final'; END IF;
    IF v_run.state='final' THEN
        IF v_run.terminal_outcome->>'answer_bundle_hash' IS DISTINCT FROM p_request->>'answer_bundle_hash' THEN
            RAISE EXCEPTION USING ERRCODE='K1011',MESSAGE='final_conflict';
        END IF;
        v_response:=jsonb_build_object(
            'outcome','already_final','run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
            'run_version',v_run.run_version,'answer_bundle_hash',p_request->>'answer_bundle_hash'
        );
        PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','commit_final',p_request->>'mutation_hash',p_request,v_response);
        RETURN v_response;
    END IF;
    IF v_run.state<>'active' THEN RAISE EXCEPTION USING ERRCODE='K1003',MESSAGE='terminal_or_inactive_run'; END IF;
    IF v_run.lease_deadline<=clock_timestamp() THEN RAISE EXCEPTION USING ERRCODE='K1013',MESSAGE='lease_lost'; END IF;
    IF v_run.run_version<>agent_store.request_bigint(p_request,'expected_run_version',1,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE='K1012',MESSAGE='run_version_mismatch'; END IF;
    IF v_run.cancel_generation<>agent_store.request_bigint(p_request,'expected_cancel_generation',0,9223372036854775807) THEN RAISE EXCEPTION USING ERRCODE='K1009',MESSAGE='cancel_generation_mismatch'; END IF;
    IF EXISTS (
        SELECT 1 FROM agent_store.actions WHERE run_id=v_run.run_id AND stage IN ('begun','observed')
    ) THEN
        RAISE EXCEPTION USING ERRCODE='K1016',MESSAGE='pending_action';
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
            'rendered_message_hash',p_request->>'rendered_message_hash'
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
        'run_version',v_run.run_version,'answer_bundle_hash',p_request->>'answer_bundle_hash'
    );
    PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','commit_final',p_request->>'mutation_hash',p_request,v_response);
    RETURN v_response;
END;
$$;

CREATE OR REPLACE FUNCTION agent_v1.request_cancel(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
    v_replay jsonb;
    v_response jsonb;
    v_outcome text;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','reason_code','release'],
        ARRAY['abi_version','mutation_id','mutation_hash','run_id','tenant_id','reason_code','release']
    );
    PERFORM agent_store.assert_hash(p_request->>'mutation_hash');
    PERFORM agent_store.assert_text(p_request,'mutation_id',128);
    PERFORM agent_store.assert_text(p_request,'run_id',128);
    PERFORM agent_store.assert_text(p_request,'tenant_id',128);
    PERFORM agent_store.assert_text(p_request,'reason_code',64);
    PERFORM agent_store.assert_json_size(p_request->'release',1048576);
    SELECT * INTO v_run FROM agent_store.runs WHERE run_id=p_request->>'run_id' FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch'; END IF;
    v_replay:=agent_store.replay_mutation(v_run.run_id,p_request->>'mutation_id','request_cancel',p_request->>'mutation_hash',p_request);
    IF v_replay IS NOT NULL THEN RETURN v_replay; END IF;

    IF v_run.state='final' THEN
        v_outcome:='already_final';
    ELSIF v_run.state='cancelled' THEN
        v_outcome:='already_cancelled';
    ELSIF v_run.state='failed' THEN
        v_outcome:='already_failed';
    ELSE
        UPDATE agent_store.runs
           SET state='cancelled',run_version=run_version+1,
               cancel_generation=cancel_generation+1,lease_owner=NULL,lease_deadline=NULL,
               terminal_outcome=jsonb_build_object(
                   'kind','cancelled','reason_code',p_request->>'reason_code'
               ),updated_at=clock_timestamp()
         WHERE run_id=v_run.run_id RETURNING * INTO v_run;
        INSERT INTO agent_store.settlements (run_id,settlement_kind,settlement_payload)
        VALUES (v_run.run_id,'released',p_request->'release');
        INSERT INTO agent_store.outbox (run_id,event_kind,dedupe_key,payload)
        VALUES (
            v_run.run_id,'run.cancelled',v_run.run_id || ':run.cancelled',
            jsonb_build_object(
                'run_id',v_run.run_id,'cancel_generation',v_run.cancel_generation,
                'reason_code',p_request->>'reason_code'
            )
        );
        v_outcome:='cancelled';
    END IF;
    v_response:=jsonb_build_object(
        'outcome',v_outcome,'run_id',v_run.run_id,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,'cancel_generation',v_run.cancel_generation
    );
    PERFORM agent_store.record_mutation(v_run.run_id,p_request->>'mutation_id','request_cancel',p_request->>'mutation_hash',p_request,v_response);
    RETURN v_response;
END;
$$;

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
        SELECT allocated.queue_start_tag, allocated.queue_finish_tag
          INTO v_queue_start_tag, v_queue_finish_tag
          FROM agent_store.allocate_fair_queue_tag(
              v_run.tenant_id, v_run.principal_id
          ) AS allocated;
        UPDATE agent_store.runs
           SET state='deferred',run_version=run_version+1,lease_owner=NULL,lease_deadline=NULL,
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

CREATE OR REPLACE FUNCTION agent_v1.claim_outbox(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_worker_id text;
    v_limit bigint;
    v_lease_ms bigint;
    v_events jsonb;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','worker_id','limit','lease_ms'],
        ARRAY['abi_version','worker_id','limit','lease_ms']
    );
    PERFORM agent_store.assert_text(p_request,'worker_id',128);
    v_worker_id:=p_request->>'worker_id';
    v_limit:=agent_store.request_bigint(p_request,'limit',1,100);
    v_lease_ms:=agent_store.request_bigint(p_request,'lease_ms',1000,600000);

    WITH candidates AS (
        SELECT outbox_id
          FROM agent_store.outbox
         WHERE delivered_at IS NULL
           AND (delivery_owner IS NULL OR delivery_deadline<=clock_timestamp())
         ORDER BY outbox_id
         FOR UPDATE SKIP LOCKED
         LIMIT v_limit
    ), claimed AS (
        UPDATE agent_store.outbox AS outbox
           SET delivery_owner=v_worker_id,
               delivery_deadline=clock_timestamp()+v_lease_ms*interval '1 millisecond',
               delivery_attempts=delivery_attempts+1
          FROM candidates
         WHERE outbox.outbox_id=candidates.outbox_id
         RETURNING outbox.outbox_id,outbox.run_id,outbox.event_kind,outbox.dedupe_key,
                   outbox.payload,outbox.delivery_attempts,outbox.delivery_deadline
    )
    SELECT COALESCE(jsonb_agg(jsonb_build_object(
        'outbox_id',outbox_id,'run_id',run_id,'event_kind',event_kind,
        'dedupe_key',dedupe_key,'payload',payload,'delivery_attempts',delivery_attempts,
        'delivery_deadline',delivery_deadline
    ) ORDER BY outbox_id),'[]'::jsonb) INTO v_events FROM claimed;
    RETURN jsonb_build_object('events',v_events);
END;
$$;

CREATE OR REPLACE FUNCTION agent_v1.ack_outbox(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_outbox agent_store.outbox%ROWTYPE;
    v_success boolean;
    v_error_hash text;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','worker_id','outbox_id','success','error_hash'],
        ARRAY['abi_version','worker_id','outbox_id','success']
    );
    PERFORM agent_store.assert_text(p_request,'worker_id',128);
    v_success:=agent_store.request_boolean(p_request,'success');
    v_error_hash:=p_request->>'error_hash';
    IF v_success AND v_error_hash IS NOT NULL THEN
        RAISE EXCEPTION USING ERRCODE='K1000',MESSAGE='successful_ack_cannot_have_error';
    END IF;
    IF NOT v_success THEN PERFORM agent_store.assert_hash(v_error_hash); END IF;

    SELECT * INTO v_outbox FROM agent_store.outbox
     WHERE outbox_id=agent_store.request_bigint(p_request,'outbox_id',1,9223372036854775807)
     FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1019',MESSAGE='unknown_outbox_event'; END IF;
    IF v_outbox.delivered_at IS NOT NULL THEN
        RETURN jsonb_build_object('outcome','already_delivered','outbox_id',v_outbox.outbox_id);
    END IF;
    IF NOT v_success AND v_outbox.delivery_owner IS NULL
       AND v_outbox.last_error_hash IS NOT DISTINCT FROM v_error_hash THEN
        RETURN jsonb_build_object('outcome','already_released','outbox_id',v_outbox.outbox_id);
    END IF;
    IF v_outbox.delivery_owner IS DISTINCT FROM p_request->>'worker_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1020',MESSAGE='outbox_lease_lost';
    END IF;
    IF v_success THEN
        UPDATE agent_store.outbox
           SET delivered_at=clock_timestamp(),delivery_owner=NULL,delivery_deadline=NULL
         WHERE outbox_id=v_outbox.outbox_id;
        RETURN jsonb_build_object('outcome','delivered','outbox_id',v_outbox.outbox_id);
    END IF;
    UPDATE agent_store.outbox
       SET delivery_owner=NULL,delivery_deadline=NULL,last_error_hash=v_error_hash
     WHERE outbox_id=v_outbox.outbox_id;
    RETURN jsonb_build_object('outcome','released','outbox_id',v_outbox.outbox_id);
END;
$$;

CREATE OR REPLACE FUNCTION agent_v1.read_committed_outcome(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $$
DECLARE
    v_run agent_store.runs%ROWTYPE;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','run_id','tenant_id'],
        ARRAY['abi_version','run_id','tenant_id']
    );
    SELECT * INTO v_run FROM agent_store.runs WHERE run_id=p_request->>'run_id';
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='K1001',MESSAGE='unknown_run'; END IF;
    IF v_run.tenant_id IS DISTINCT FROM p_request->>'tenant_id' THEN
        RAISE EXCEPTION USING ERRCODE='K1017',MESSAGE='tenant_mismatch';
    END IF;
    RETURN jsonb_build_object(
        'run_id',v_run.run_id,'state',v_run.state,'fencing_token',v_run.fencing_token,
        'run_version',v_run.run_version,
        'cancel_generation',v_run.cancel_generation,'terminal_outcome',v_run.terminal_outcome
    );
END;
$$;

REVOKE ALL ON ALL TABLES IN SCHEMA agent_store FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA agent_store FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA agent_store FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA agent_v1 FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA agent_store REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA agent_store REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA agent_store REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA agent_v1 REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;

COMMENT ON SCHEMA agent_store IS
    'Private agent persistence tables. Runtime roles must never receive direct privileges.';
COMMENT ON SCHEMA agent_v1 IS
    'Version 1 EXECUTE-only JSON procedure ABI for KRW agent persistence.';
COMMENT ON TABLE agent_store.provider_episodes IS
    'Stores replay hashes and encrypted/CAS artifact references, never raw reasoning content.';
COMMENT ON TABLE agent_store.run_state_checkpoints IS
    'Stores only bounded encrypted/CAS runtime-state receipts pinned to durable execution frontiers.';
COMMENT ON TABLE agent_store.outbox IS
    'Transactional outbox committed atomically with terminal result and settlement.';

COMMIT;
