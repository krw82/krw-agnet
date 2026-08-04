CREATE FUNCTION pg_temp.test_hash(p_value text) RETURNS text
LANGUAGE sql IMMUTABLE STRICT SET search_path = pg_catalog
AS $$
  SELECT 'sha256:' || encode(sha256(convert_to(p_value, 'UTF8')), 'hex')
$$;

CREATE FUNCTION pg_temp.test_message_id(p_run_id text, p_role text) RETURNS text
LANGUAGE sql IMMUTABLE STRICT SET search_path = pg_catalog
AS $$
  SELECT 'sha256:' || encode(sha256(
    convert_to('memory-message/v2', 'UTF8') || decode('00', 'hex') ||
    convert_to(p_run_id, 'UTF8') || decode('00', 'hex') || convert_to(p_role, 'UTF8')
  ), 'hex')
$$;

CREATE FUNCTION pg_temp.test_final_intent(p_run_id text, p_bundle_hash text) RETURNS text
LANGUAGE sql IMMUTABLE STRICT SET search_path = pg_catalog
AS $$
  SELECT 'sha256:' || encode(sha256(
    convert_to('krw.final-commit-intent/v2', 'UTF8') || decode('00', 'hex') ||
    convert_to(p_run_id, 'UTF8') || decode('00', 'hex') ||
    convert_to(p_bundle_hash, 'UTF8')
  ), 'hex')
$$;

CREATE FUNCTION pg_temp.add_active_run(
  p_run_id text,
  p_session_id text,
  p_fence bigint,
  p_version bigint
) RETURNS void
LANGUAGE sql VOLATILE
AS $$
  INSERT INTO agent_store.runs(
    run_id, tenant_id, principal_id, session_id, state, fencing_token, run_version,
    lease_owner, lease_deadline, agent_image_hash, runtime_version,
    immutable_snapshot_hash, immutable_snapshot, resource_profile, budgets
  ) VALUES (
    p_run_id, 'tenant-a', 'principal-a', p_session_id, 'active', p_fence, p_version,
    'worker-test', clock_timestamp() + interval '10 minutes',
    pg_temp.test_hash('image'), '0.1.0', pg_temp.test_hash('snapshot-' || p_run_id),
    '{}'::jsonb, '{}'::jsonb, '{}'::jsonb
  )
$$;

CREATE FUNCTION pg_temp.memory_delta_v2(
  p_run_id text,
  p_session_id text,
  p_revision bigint,
  p_parent_frontier text,
  p_bundle_hash text,
  p_final_output_hash text
) RETURNS jsonb
LANGUAGE sql IMMUTABLE
AS $$
  SELECT jsonb_build_object(
    'schema_version', 3,
    'authority', 'context_only',
    'session_id_hash', pg_temp.test_hash(p_session_id),
    'parent_frontier_hash', p_parent_frontier,
    'revision', p_revision,
    'source', jsonb_build_object(
      'run_id', p_run_id,
      'revision', p_revision,
      'user_message_id', pg_temp.test_message_id(p_run_id, 'user'),
      'assistant_message_id', pg_temp.test_message_id(p_run_id, 'assistant'),
      'final_commit_intent_hash', pg_temp.test_final_intent(p_run_id, p_bundle_hash),
      'answer_bundle_hash', p_bundle_hash,
      'final_output_hash', p_final_output_hash
    ),
    'tickers', '[]'::jsonb,
    'constraints', '[]'::jsonb,
    'claims', '[]'::jsonb,
    'unresolved_goals', '[]'::jsonb,
    'supersessions', '[]'::jsonb,
    'resolved_goals', '[]'::jsonb,
    'recent_turn', jsonb_build_object(
      'source_run_id', p_run_id,
      'user_content_hash', pg_temp.test_hash('question'),
      'user_content', 'question',
      'user_content_original_bytes', 8,
      'user_content_truncated', false,
      'answer_content_hash', pg_temp.test_hash('answer'),
      'answer_content', 'answer',
      'answer_content_original_bytes', 6,
      'answer_content_truncated', false
    )
  )
$$;

CREATE FUNCTION pg_temp.final_request(
  p_run_id text,
  p_fence bigint,
  p_version bigint,
  p_bundle_hash text,
  p_final_output_hash text,
  p_delta_hash text,
  p_delta jsonb,
  p_next_frontier text
) RETURNS jsonb
LANGUAGE sql IMMUTABLE
AS $$
  SELECT jsonb_build_object(
    'abi_version', 1,
    'mutation_id', 'commit-' || p_run_id,
    'mutation_hash', pg_temp.test_hash('request-' || p_run_id),
    'run_id', p_run_id,
    'tenant_id', 'tenant-a',
    'fencing_token', p_fence,
    'expected_run_version', p_version,
    'expected_cancel_generation', 0,
    'answer_bundle_hash', p_bundle_hash,
    'final_output_hash', p_final_output_hash,
    'rendered_message_hash', pg_temp.test_hash('rendered-' || p_run_id),
    'answer_bundle', jsonb_build_object('schema_version', 3),
    'usage', jsonb_build_object('provider_turns', 2),
    'settlement', jsonb_build_object('kind', 'usage_settled'),
    'outbox_payloads', jsonb_build_object(
      'presentation', jsonb_build_object('answer_bundle_hash', p_bundle_hash)
    ),
    'session_memory_delta_hash', p_delta_hash,
    'session_memory_delta', p_delta,
    'next_memory_frontier_hash', p_next_frontier
  )
$$;

DO $test$
DECLARE
  v_empty constant text :=
    'sha256:a4e1b26a1a1e27b135aa73cb802316af633c59cc50e92358bca73d6c4e6a4fbb';
  v_empty_source constant text :=
    'sha256:e0d62521a0b7c1b8d1f3ba5e090c6a7bd60261c57adae0f7ba496d4b06216d39';
  v_bundle text := pg_temp.test_hash('bundle-source');
  v_ir text := pg_temp.test_hash('ir-source');
  v_delta_hash text := pg_temp.test_hash('delta-source');
  v_delta jsonb;
  v_next text;
  v_source_lineage text;
  v_request jsonb;
  v_response jsonb;
BEGIN
  IF agent_store.session_memory_frontier_hash(
    'sha256:' || repeat('1', 64), 7, 'sha256:' || repeat('2', 64)
  ) <> 'sha256:f2655508c5e2c43af0292f6e230fa57188befd420b2dc24c204d0b3a36744b42' THEN
    RAISE EXCEPTION 'PostgreSQL frontier hash differs from the Rust vector';
  END IF;
  IF agent_store.session_memory_source_lineage_hash(
    'sha256:' || repeat('1', 64), 7, 'sha256:' || repeat('2', 64)
  ) <> 'sha256:f38c86974bb7db4cb455248dfd43fe4b9011f63ba5f87b50523dbd3fc3823307' THEN
    RAISE EXCEPTION 'PostgreSQL source lineage hash differs from the Rust vector';
  END IF;

  PERFORM pg_temp.add_active_run('memory-source-run', 'session-a', 7, 3);
  v_delta := pg_temp.memory_delta_v2(
    'memory-source-run', 'session-a', 1, v_empty, v_bundle, v_ir
  );
  v_next := agent_store.session_memory_frontier_hash(v_empty, 1, v_delta_hash);
  v_source_lineage := agent_store.session_memory_source_lineage_hash(
    v_empty_source, 1, pg_temp.test_hash('memory-source-run')
  );
  v_request := pg_temp.final_request(
    'memory-source-run', 7, 3, v_bundle, v_ir, v_delta_hash, v_delta, v_next
  );
  v_response := agent_v1.commit_final(v_request);
  IF v_response->>'outcome' <> 'final_committed'
     OR (v_response->>'memory_revision')::bigint <> 1
     OR v_response->>'memory_frontier_hash' <> v_next
     OR v_response->>'memory_source_lineage_hash' <> v_source_lineage
     OR agent_v1.commit_final(v_request) IS DISTINCT FROM v_response THEN
    RAISE EXCEPTION 'memory final or exact replay failed';
  END IF;
  IF (SELECT count(*) FROM agent_store.session_memory_deltas
       WHERE source_run_id='memory-source-run') <> 1
     OR (SELECT revision FROM agent_store.session_memory_frontiers
          WHERE tenant_id='tenant-a' AND principal_id='principal-a'
            AND session_id='session-a') <> 1
     OR (SELECT source_lineage_hash FROM agent_store.session_memory_frontiers
          WHERE tenant_id='tenant-a' AND principal_id='principal-a'
            AND session_id='session-a') <> v_source_lineage
     OR (SELECT source_lineage_hash FROM agent_store.session_memory_deltas
          WHERE source_run_id='memory-source-run') <> v_source_lineage
     OR (SELECT state FROM agent_store.runs WHERE run_id='memory-source-run') <> 'final'
     OR (SELECT count(*) FROM agent_store.answer_bundles
          WHERE run_id='memory-source-run') <> 1 THEN
    RAISE EXCEPTION 'memory final was not atomic';
  END IF;
  IF EXISTS (
    SELECT 1 FROM agent_store.outbox
     WHERE run_id='memory-source-run'
       AND payload::text ~ 'session_memory_delta(_hash)?|next_memory_frontier_hash'
  ) OR NOT EXISTS (
    SELECT 1 FROM agent_store.outbox
     WHERE run_id='memory-source-run' AND event_kind='answer.committed'
       AND (payload->>'memory_revision')::bigint=1
       AND payload->>'memory_frontier_hash'=v_next
       AND payload->>'memory_source_lineage_hash'=v_source_lineage
  ) THEN
    RAISE EXCEPTION 'outbox memory minimization failed';
  END IF;
  BEGIN
    UPDATE agent_store.session_memory_deltas SET delta_hash=delta_hash
     WHERE source_run_id='memory-source-run';
    RAISE EXCEPTION 'append-only mutation unexpectedly succeeded';
  EXCEPTION WHEN SQLSTATE 'K1025' THEN
    NULL;
  END;
END;
$test$;

DO $test$
DECLARE
  v_page jsonb;
BEGIN
  PERFORM pg_temp.add_active_run('memory-reader-run', 'session-a', 11, 2);
  v_page := agent_v1.read_session_memory(jsonb_build_object(
    'abi_version', 1, 'run_id', 'memory-reader-run', 'tenant_id', 'tenant-a',
    'principal_id', 'principal-a', 'session_id', 'session-a', 'fencing_token', 11,
    'after_revision', 0, 'limit', 1, 'mode', 'audit_rebuild'
  ));
  IF (v_page->>'memory_revision')::bigint <> 1
     OR jsonb_array_length(v_page->'deltas') <> 1
     OR v_page->>'memory_source_lineage_hash'
          <> v_page->'deltas'->0->>'source_lineage_hash'
     OR (v_page->>'next_after_revision')::bigint <> 1
     OR (v_page->>'has_more')::boolean THEN
    RAISE EXCEPTION 'bounded ordered memory page failed';
  END IF;
  BEGIN
    PERFORM agent_v1.read_session_memory(jsonb_build_object(
      'abi_version', 1, 'run_id', 'memory-reader-run', 'tenant_id', 'tenant-a',
      'principal_id', 'principal-b', 'session_id', 'session-a', 'fencing_token', 11,
      'after_revision', 0, 'limit', 1, 'mode', 'audit_rebuild'
    ));
    RAISE EXCEPTION 'wrong principal unexpectedly read memory';
  EXCEPTION WHEN SQLSTATE 'K1026' THEN
    NULL;
  END;
  BEGIN
    PERFORM agent_v1.read_session_memory(jsonb_build_object(
      'abi_version', 1, 'run_id', 'memory-reader-run', 'tenant_id', 'tenant-a',
      'principal_id', 'principal-a', 'session_id', 'session-a', 'fencing_token', 10,
      'after_revision', 0, 'limit', 1, 'mode', 'audit_rebuild'
    ));
    RAISE EXCEPTION 'stale fence unexpectedly read memory';
  EXCEPTION WHEN SQLSTATE 'K1002' THEN
    NULL;
  END;
  UPDATE agent_store.runs
     SET state='failed', terminal_outcome='{"kind":"test_complete"}'::jsonb,
         lease_owner=NULL, lease_deadline=NULL
   WHERE run_id='memory-reader-run';
END;
$test$;

DO $test$
DECLARE
  v_empty constant text :=
    'sha256:a4e1b26a1a1e27b135aa73cb802316af633c59cc50e92358bca73d6c4e6a4fbb';
  v_bundle text := pg_temp.test_hash('bundle-stale');
  v_ir text := pg_temp.test_hash('ir-stale');
  v_delta_hash text := pg_temp.test_hash('delta-stale');
  v_delta jsonb;
BEGIN
  PERFORM pg_temp.add_active_run('memory-stale-run', 'session-a', 12, 1);
  v_delta := pg_temp.memory_delta_v2(
    'memory-stale-run', 'session-a', 1, v_empty, v_bundle, v_ir
  );
  BEGIN
    PERFORM agent_v1.commit_final(pg_temp.final_request(
      'memory-stale-run', 12, 1, v_bundle, v_ir, v_delta_hash, v_delta,
      agent_store.session_memory_frontier_hash(v_empty, 1, v_delta_hash)
    ));
    RAISE EXCEPTION 'stale parent unexpectedly committed';
  EXCEPTION WHEN SQLSTATE 'K1025' THEN
    NULL;
  END;
  IF EXISTS (SELECT 1 FROM agent_store.answer_bundles WHERE run_id='memory-stale-run')
     OR EXISTS (SELECT 1 FROM agent_store.session_memory_deltas
                 WHERE source_run_id='memory-stale-run')
     OR (SELECT state FROM agent_store.runs WHERE run_id='memory-stale-run') <> 'active' THEN
    RAISE EXCEPTION 'frontier conflict was not atomic';
  END IF;
  UPDATE agent_store.runs
     SET state='failed', terminal_outcome='{"kind":"test_complete"}'::jsonb,
         lease_owner=NULL, lease_deadline=NULL
   WHERE run_id='memory-stale-run';
END;
$test$;

DO $test$
DECLARE
  v_parent text;
  v_bundle text := pg_temp.test_hash('bundle-rollback');
  v_ir text := pg_temp.test_hash('ir-rollback');
  v_delta_hash text := pg_temp.test_hash('delta-rollback');
  v_delta jsonb;
BEGIN
  SELECT frontier_hash INTO v_parent FROM agent_store.session_memory_frontiers
   WHERE tenant_id='tenant-a' AND principal_id='principal-a' AND session_id='session-a';
  PERFORM pg_temp.add_active_run('memory-rollback-run', 'session-a', 13, 1);
  INSERT INTO agent_store.outbox(run_id, event_kind, dedupe_key, payload)
  VALUES (
    'memory-rollback-run', 'answer.committed',
    'memory-rollback-run:answer.committed', '{"preexisting":true}'::jsonb
  );
  v_delta := pg_temp.memory_delta_v2(
    'memory-rollback-run', 'session-a', 2, v_parent, v_bundle, v_ir
  );
  BEGIN
    PERFORM agent_v1.commit_final(pg_temp.final_request(
      'memory-rollback-run', 13, 1, v_bundle, v_ir, v_delta_hash, v_delta,
      agent_store.session_memory_frontier_hash(v_parent, 2, v_delta_hash)
    ));
    RAISE EXCEPTION 'forced post-memory failure unexpectedly committed';
  EXCEPTION WHEN unique_violation THEN
    NULL;
  END;
  IF EXISTS (SELECT 1 FROM agent_store.session_memory_deltas
              WHERE source_run_id='memory-rollback-run')
     OR EXISTS (SELECT 1 FROM agent_store.answer_bundles
                 WHERE run_id='memory-rollback-run')
     OR (SELECT revision FROM agent_store.session_memory_frontiers
          WHERE tenant_id='tenant-a' AND principal_id='principal-a'
            AND session_id='session-a') <> 1
     OR (SELECT state FROM agent_store.runs WHERE run_id='memory-rollback-run') <> 'active' THEN
    RAISE EXCEPTION 'final transaction did not roll memory back';
  END IF;
  UPDATE agent_store.runs
     SET state='failed', terminal_outcome='{"kind":"test_complete"}'::jsonb,
         lease_owner=NULL, lease_deadline=NULL
   WHERE run_id='memory-rollback-run';
END;
$test$;

DO $test$
DECLARE
  v_frontier text;
  v_source_lineage text;
  v_snapshot jsonb;
  v_snapshot_hash text;
  v_snapshot_size bigint;
  v_checkpoint_request jsonb;
  v_checkpoint_response jsonb;
  v_bundle text := pg_temp.test_hash('bundle-tail');
  v_ir text := pg_temp.test_hash('ir-tail');
  v_delta_hash text := pg_temp.test_hash('delta-tail');
  v_delta jsonb;
  v_next text;
  v_next_source_lineage text;
  v_page jsonb;
BEGIN
  SELECT frontier_hash,source_lineage_hash
    INTO v_frontier,v_source_lineage
    FROM agent_store.session_memory_frontiers
   WHERE tenant_id='tenant-a' AND principal_id='principal-a'
     AND session_id='session-a';
  SELECT delta INTO v_delta
    FROM agent_store.session_memory_deltas
   WHERE source_run_id='memory-source-run';
  v_snapshot := jsonb_build_object(
    'schema_version', 3,
    'authority', 'context_only',
    'session_id_hash', pg_temp.test_hash('session-a'),
    'revision', 1,
    'frontier_hash', v_frontier,
    'source_lineage_hash', v_source_lineage,
    'recent_source_revision_index',
      jsonb_build_object(pg_temp.test_hash('memory-source-run'), 1),
    'constraint_hashes', '{}'::jsonb,
    'active_claim_fingerprints', '{}'::jsonb,
    'unresolved_goal_ids', '[]'::jsonb,
    'tickers', '[]'::jsonb,
    'constraints', '[]'::jsonb,
    'sources', jsonb_build_object('memory-source-run', v_delta->'source'),
    'claims', '[]'::jsonb,
    'unresolved_goals', '[]'::jsonb,
    'recent_turns', jsonb_build_array(v_delta->'recent_turn')
  );
  v_snapshot_hash := pg_temp.test_hash(v_snapshot::text);
  v_snapshot_size := octet_length(convert_to(v_snapshot::text, 'UTF8'));
  PERFORM pg_temp.add_active_run('memory-checkpoint-run', 'session-a', 14, 1);
  v_checkpoint_request := jsonb_build_object(
    'abi_version', 1,
    'mutation_id', 'checkpoint-session-a-1',
    'mutation_hash', pg_temp.test_hash('checkpoint-session-a-1'),
    'run_id', 'memory-checkpoint-run',
    'tenant_id', 'tenant-a',
    'principal_id', 'principal-a',
    'session_id', 'session-a',
    'fencing_token', 14,
    'expected_run_version', 1,
    'snapshot_revision', 1,
    'frontier_hash', v_frontier,
    'source_lineage_hash', v_source_lineage,
    'snapshot_hash', v_snapshot_hash,
    'snapshot_size_bytes', v_snapshot_size,
    'snapshot', v_snapshot
  );
  v_checkpoint_response := agent_v1.checkpoint_session_memory_snapshot(
    v_checkpoint_request
  );
  IF v_checkpoint_response->>'outcome' <> 'snapshot_checkpointed'
     OR (v_checkpoint_response->>'run_version')::bigint <> 2
     OR v_checkpoint_response->>'source_lineage_hash' <> v_source_lineage
     OR agent_v1.checkpoint_session_memory_snapshot(v_checkpoint_request)
          IS DISTINCT FROM v_checkpoint_response THEN
    RAISE EXCEPTION 'snapshot checkpoint or exact replay failed';
  END IF;

  BEGIN
    PERFORM agent_v1.checkpoint_session_memory_snapshot(jsonb_set(
      v_checkpoint_request, '{mutation_hash}',
      to_jsonb(pg_temp.test_hash('conflicting-checkpoint-mutation'))
    ));
    RAISE EXCEPTION 'conflicting checkpoint mutation unexpectedly replayed';
  EXCEPTION WHEN SQLSTATE 'K1004' THEN
    NULL;
  END;
  BEGIN
    PERFORM agent_v1.checkpoint_session_memory_snapshot(
      jsonb_set(
        jsonb_set(
          jsonb_set(
            v_checkpoint_request,
            '{mutation_id}', to_jsonb('checkpoint-stale-lineage'::text)
          ),
          '{mutation_hash}', to_jsonb(pg_temp.test_hash('checkpoint-stale-lineage'))
        ),
        '{expected_run_version}', '2'::jsonb
      ) || jsonb_build_object('source_lineage_hash', pg_temp.test_hash('wrong-lineage'))
    );
    RAISE EXCEPTION 'wrong source lineage unexpectedly checkpointed';
  EXCEPTION WHEN SQLSTATE 'K1025' THEN
    NULL;
  END;
  BEGIN
    PERFORM agent_v1.checkpoint_session_memory_snapshot(
      jsonb_set(v_checkpoint_request, '{principal_id}', to_jsonb('principal-b'::text))
    );
    RAISE EXCEPTION 'wrong principal unexpectedly checkpointed';
  EXCEPTION WHEN SQLSTATE 'K1026' THEN
    NULL;
  END;
  BEGIN
    PERFORM agent_v1.checkpoint_session_memory_snapshot(
      jsonb_set(v_checkpoint_request, '{fencing_token}', '13'::jsonb)
    );
    RAISE EXCEPTION 'stale fence unexpectedly checkpointed';
  EXCEPTION WHEN SQLSTATE 'K1002' THEN
    NULL;
  END;
  BEGIN
    PERFORM agent_v1.checkpoint_session_memory_snapshot(
      jsonb_set(v_checkpoint_request, '{snapshot_size_bytes}', '4194305'::jsonb)
    );
    RAISE EXCEPTION 'oversized snapshot declaration unexpectedly checkpointed';
  EXCEPTION WHEN SQLSTATE 'K1000' THEN
    NULL;
  END;
  BEGIN
    PERFORM agent_v1.checkpoint_session_memory_snapshot(
      jsonb_set(
        jsonb_set(
          jsonb_set(
            jsonb_set(
              v_checkpoint_request,
              '{mutation_id}', to_jsonb('checkpoint-bad-recent-index'::text)
            ),
            '{mutation_hash}', to_jsonb(pg_temp.test_hash('checkpoint-bad-recent-index'))
          ),
          '{expected_run_version}', '2'::jsonb
        ),
        '{snapshot,recent_source_revision_index}',
        jsonb_build_object(pg_temp.test_hash('memory-source-run'), 2)
      )
    );
    RAISE EXCEPTION 'invalid recent source suffix unexpectedly checkpointed';
  EXCEPTION WHEN SQLSTATE 'K1025' THEN
    NULL;
  END;
  BEGIN
    UPDATE agent_store.session_memory_snapshots
       SET snapshot_hash=snapshot_hash
     WHERE checkpoint_run_id='memory-checkpoint-run';
    RAISE EXCEPTION 'snapshot append-only mutation unexpectedly succeeded';
  EXCEPTION WHEN SQLSTATE 'K1025' THEN
    NULL;
  END;

  -- The production queue permits only one active owner for a session. Release
  -- the checkpoint worker before a later run advances the same memory head.
  UPDATE agent_store.runs
     SET state='failed', terminal_outcome='{"kind":"test_complete"}'::jsonb,
         lease_owner=NULL, lease_deadline=NULL
   WHERE run_id='memory-checkpoint-run';

  PERFORM pg_temp.add_active_run('memory-tail-run', 'session-a', 15, 1);
  v_delta := pg_temp.memory_delta_v2(
    'memory-tail-run', 'session-a', 2, v_frontier, v_bundle, v_ir
  );
  v_next := agent_store.session_memory_frontier_hash(
    v_frontier, 2, v_delta_hash
  );
  v_next_source_lineage := agent_store.session_memory_source_lineage_hash(
    v_source_lineage, 2, pg_temp.test_hash('memory-tail-run')
  );
  PERFORM agent_v1.commit_final(pg_temp.final_request(
    'memory-tail-run', 15, 1, v_bundle, v_ir, v_delta_hash, v_delta, v_next
  ));

  -- A subsequent active owner reads the checkpoint + tail; it cannot rely on
  -- the now-terminal writer's lease or fence.
  PERFORM pg_temp.add_active_run('memory-read-run', 'session-a', 16, 1);

  v_page := agent_v1.read_session_memory(jsonb_build_object(
    'abi_version', 1,
    'run_id', 'memory-read-run',
    'tenant_id', 'tenant-a',
    'principal_id', 'principal-a',
    'session_id', 'session-a',
    'fencing_token', 16,
    'after_revision', 0,
    'limit', 8,
    'mode', 'snapshot_tail'
  ));
  IF (v_page->>'memory_revision')::bigint <> 2
     OR v_page->>'memory_frontier_hash' <> v_next
     OR v_page->>'memory_source_lineage_hash' <> v_next_source_lineage
     OR (v_page->'snapshot'->>'revision')::bigint <> 1
     OR v_page->'snapshot'->>'source_lineage_hash' <> v_source_lineage
     OR jsonb_array_length(v_page->'deltas') <> 1
     OR (v_page->'deltas'->0->>'revision')::bigint <> 2
     OR v_page->'deltas'->0->>'source_lineage_hash' <> v_next_source_lineage
     OR (v_page->>'next_after_revision')::bigint <> 2
     OR (v_page->>'has_more')::boolean THEN
    RAISE EXCEPTION 'snapshot plus bounded tail restore failed';
  END IF;
  v_page := agent_v1.read_session_memory(jsonb_build_object(
    'abi_version', 1,
    'run_id', 'memory-read-run',
    'tenant_id', 'tenant-a',
    'principal_id', 'principal-a',
    'session_id', 'session-a',
    'fencing_token', 16,
    'after_revision', 0,
    'limit', 8,
    'mode', 'audit_rebuild'
  ));
  IF v_page->'snapshot' <> 'null'::jsonb
     OR jsonb_array_length(v_page->'deltas') <> 2
     OR v_page->'deltas'->1->>'source_lineage_hash' <> v_next_source_lineage THEN
    RAISE EXCEPTION 'append-only audit rebuild failed after snapshot';
  END IF;
  UPDATE agent_store.runs
     SET state='failed', terminal_outcome='{"kind":"test_complete"}'::jsonb,
         lease_owner=NULL, lease_deadline=NULL
   WHERE run_id='memory-read-run';
END;
$test$;

DO $test$
DECLARE
  v_response jsonb;
BEGIN
  PERFORM pg_temp.add_active_run('product-run', 'product-session', 20, 1);
  v_response := agent_v1.commit_final(pg_temp.final_request(
    'product-run', 20, 1, pg_temp.test_hash('product-bundle'),
    pg_temp.test_hash('product-ir'), NULL, NULL, NULL
  ));
  IF v_response->>'outcome' <> 'final_committed'
     OR v_response->'memory_revision' <> 'null'::jsonb
     OR v_response->'memory_frontier_hash' <> 'null'::jsonb
     OR v_response->'memory_source_lineage_hash' <> 'null'::jsonb
     OR EXISTS (
       SELECT 1 FROM agent_store.session_memory_frontiers
        WHERE tenant_id='tenant-a' AND principal_id='principal-a'
          AND session_id='product-session'
     ) THEN
    RAISE EXCEPTION 'memory-free product final violated nullable contract';
  END IF;
END;
$test$;

CREATE ROLE krw_session_memory_runtime NOLOGIN;
GRANT USAGE ON SCHEMA agent_v1 TO krw_session_memory_runtime;
GRANT EXECUTE ON FUNCTION agent_v1.read_session_memory(jsonb)
  TO krw_session_memory_runtime;

DO $test$
BEGIN
  IF has_schema_privilege('krw_session_memory_runtime', 'agent_store', 'USAGE')
     OR has_table_privilege(
       'krw_session_memory_runtime', 'agent_store.session_memory_deltas', 'SELECT'
     )
     OR NOT has_function_privilege(
       'krw_session_memory_runtime', 'agent_v1.read_session_memory(jsonb)', 'EXECUTE'
     ) THEN
    RAISE EXCEPTION 'EXECUTE-only runtime privilege boundary failed';
  END IF;
END;
$test$;
