-- Create the retention tombstone before a live run writes its first session
-- memory snapshot. A snapshot is checkpointed before the run's final delta,
-- so the delta trigger from 0016 cannot create this source row in time.

BEGIN;

CREATE FUNCTION agent_store.capture_session_memory_snapshot_source() RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store
AS $function$
BEGIN
    INSERT INTO agent_store.session_memory_sources(
        run_id, tenant_id, principal_id, session_id
    ) VALUES (
        NEW.checkpoint_run_id, NEW.tenant_id, NEW.principal_id, NEW.session_id
    )
    ON CONFLICT (run_id) DO NOTHING;
    RETURN NEW;
END;
$function$;

CREATE TRIGGER session_memory_snapshot_source_tombstone_before_insert
    BEFORE INSERT ON agent_store.session_memory_snapshots
    FOR EACH ROW EXECUTE FUNCTION agent_store.capture_session_memory_snapshot_source();

REVOKE ALL ON FUNCTION agent_store.capture_session_memory_snapshot_source() FROM PUBLIC;

COMMENT ON FUNCTION agent_store.capture_session_memory_snapshot_source() IS
    'Create the immutable source tombstone required by a live snapshot checkpoint before FK validation.';

COMMIT;
