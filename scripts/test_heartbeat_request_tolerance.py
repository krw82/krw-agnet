#!/usr/bin/env python3
"""Contract test: heartbeat_daemon tolerates a pre-0023 request shape.

HERMETIC INVARIANT — this test NEVER durably modifies the shared database.
Postgres DDL is transactional, so the test loads migration 0023 with its
standalone BEGIN;/COMMIT; lines stripped and runs ALL of it (the CREATE OR
REPLACE FUNCTION plus the four contract calls) inside ONE psql session
wrapped in BEGIN; ... ROLLBACK;. The replaced function exists for the
duration of that session and vanishes on rollback. Durable application of
0023 happens only at the next production deploy via the migration
pipeline; this script never commits.

Each contract call sits inside its own SAVEPOINT so the one call that is
*supposed* to be rejected (the unknown extra field) cannot abort the
remaining calls in the shared transaction.

Run with KRW_AGENT_DATABASE_URL in the environment (pass the value through
the environment only; never write it into files, logs, or command history).

Modes:
  default        apply 0023 inside the rolled-back transaction (post-migration)
  --no-migrate   run only the contract calls against the currently deployed
                 function (pre-migration red run)
"""

import json
import os
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
MIGRATION_PATH = (
    REPO_ROOT / "migrations" / "0023_heartbeat_request_backward_compatibility.sql"
)

DB_URL = os.environ.get("KRW_AGENT_DATABASE_URL")
if not DB_URL:
    sys.exit("KRW_AGENT_DATABASE_URL is required (pass via environment only)")

BASE_REQUEST = {
    "abi_version": "1",
    "daemon_id": "tolerance-test-daemon",
    "provider": "glm",
    "descriptor_artifact_hash": "sha256:" + "0" * 64,
    "release_set_hash": "sha256:" + "0" * 64,
    "runtime_version": "0.1.0",
    "heartbeat_ttl_ms": 30000,
}


def strip_transaction_controls(sql: str) -> str:
    """Drop the migration file's standalone BEGIN;/COMMIT; lines.

    Only a line whose stripped content is exactly `BEGIN;` or `COMMIT;` is
    removed; the plpgsql BEGIN inside the dollar-quoted function body is a
    bare `BEGIN` keyword and is never touched.
    """
    kept = [
        line
        for line in sql.splitlines()
        if line.strip() not in ("BEGIN;", "COMMIT;")
    ]
    return "\n".join(kept)


def sql_literal(value: str) -> str:
    return value.replace("'", "''")


def call_block(savepoint: str, statement: str) -> str:
    return "\n".join(
        [
            f"SAVEPOINT {savepoint};",
            statement,
            f"ROLLBACK TO SAVEPOINT {savepoint};",
        ]
    )


def build_script(apply_migration: bool) -> str:
    legacy = call_block(
        "t_legacy",
        "SELECT 'legacy_ok' AS marker\n"
        " WHERE agent_v1.heartbeat_daemon('"
        + sql_literal(json.dumps(BASE_REQUEST))
        + "'::jsonb) IS NOT NULL;",
    )
    modern = call_block(
        "t_modern",
        "SELECT 'modern_mcp_ready:' || (agent_v1.heartbeat_daemon('"
        + sql_literal(json.dumps({**BASE_REQUEST, "mcp_ready": True}))
        + "'::jsonb) ->> 'mcp_ready') AS marker;",
    )
    unready = call_block(
        "t_unready",
        "SELECT 'unready_mcp_ready:' || (agent_v1.heartbeat_daemon('"
        + sql_literal(json.dumps({**BASE_REQUEST, "mcp_ready": False}))
        + "'::jsonb) ->> 'mcp_ready') AS marker;",
    )
    extra = call_block(
        "t_extra",
        "SELECT 'extra_field_accepted' AS marker\n"
        " WHERE agent_v1.heartbeat_daemon('"
        + sql_literal(
            json.dumps({**BASE_REQUEST, "mcp_ready": True, "rogue_field": 1})
        )
        + "'::jsonb) IS NOT NULL;",
    )

    parts = ["BEGIN;"]
    if apply_migration:
        if not MIGRATION_PATH.exists():
            sys.exit(f"migration file not found: {MIGRATION_PATH}")
        parts.append(strip_transaction_controls(MIGRATION_PATH.read_text()))
    parts.extend([legacy, modern, unready, extra, "ROLLBACK;", ""])
    return "\n".join(parts)


def run_session(script: str) -> subprocess.CompletedProcess:
    # The DB URL travels via argv of a directly spawned psql (no shell), the
    # same as the reference procedure; it is never echoed or written down.
    return subprocess.run(
        ["psql", DB_URL, "-v", "ON_ERROR_STOP=0", "-q", "-At"],
        input=script,
        capture_output=True,
        text=True,
    )


def main() -> int:
    apply_migration = "--no-migrate" not in sys.argv[1:]
    result = run_session(build_script(apply_migration))
    out = result.stdout
    err_snippet = " | ".join(
        line for line in result.stderr.strip().splitlines() if line
    )[:200]
    failures = []

    if "legacy_ok" not in out:
        failures.append(
            f"legacy request (no mcp_ready) was rejected: {err_snippet}"
        )

    if "modern_mcp_ready:true" not in out:
        failures.append(f"modern request (mcp_ready=true) broken: {err_snippet}")

    if "unready_mcp_ready:false" not in out:
        failures.append(f"mcp_ready=false not round-tripped: {err_snippet}")

    if "extra_field_accepted" in out:
        failures.append("unknown extra field was accepted (must stay rejected)")

    if result.returncode != 0:
        failures.append(f"psql session failed unexpectedly: {err_snippet}")

    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    mode = "migration applied inside rolled-back transaction" if apply_migration \
        else "no-migrate pre-check against deployed function"
    print(f"heartbeat request tolerance contract: PASS ({mode})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
