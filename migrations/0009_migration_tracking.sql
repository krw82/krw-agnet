BEGIN;
CREATE TABLE IF NOT EXISTS agent_store.schema_migrations (
    version integer PRIMARY KEY,
    applied_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    checksum text NOT NULL
);
-- Backfill already-applied versions (a DB that ran 0001-0008 before this
-- migration existed is marked complete so the runner does not re-apply them).
INSERT INTO agent_store.schema_migrations (version, checksum) VALUES
    (1, 'backfill'),
    (2, 'backfill'),
    (3, 'backfill'),
    (4, 'backfill'),
    (5, 'backfill'),
    (6, 'backfill'),
    (7, 'backfill'),
    (8, 'backfill'),
    (9, 'backfill')
ON CONFLICT (version) DO NOTHING;
COMMIT;
