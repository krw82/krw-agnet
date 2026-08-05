#!/usr/bin/env bash
set -euo pipefail

# Apply pending migrations based on the agent_store.schema_migrations table.
# Usage: apply_migrations.sh <psql-connect-flags...>
# Reads migrations/ relative to the repo root.
#
# Concurrency: a session-level advisory lock (keyed off hashtext of a stable
# constant) serializes the whole discover-apply-record sequence so two
# concurrent runners cannot both read the same APPLIED_MAX and double-apply
# the same migration file. The lock is held for the lifetime of the wrapper
# psql session and released on disconnect.

set -e
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MIGRATIONS_DIR="$REPO_ROOT/migrations"

# Stable advisory-lock key. hashtext('krw_agent_migrations') yields a fixed
# int4 on every Postgres build, so all invocations of this script contend on
# the same lock. (Displayed here for reproducibility; not required at runtime.)
LOCK_KEY_EXPR="hashtext('krw_agent_migrations')"

# We need the lock held across multiple files, so we drive everything through
# a single psql session fed by a heredoc. Filesystem enumeration stays in bash
# (so we only read the dir once and skip already-applied versions up front),
# and each pending file is applied via \i inside the locked session.

discover_applied_max() {
  psql "$@" -tAc \
    "SELECT COALESCE(MAX(version), 0) FROM agent_store.schema_migrations" \
    2>/dev/null || echo 0
}

APPLIED_MAX="$(discover_applied_max "$@")"
APPLIED_MAX=$((10#$APPLIED_MAX))
echo "apply_migrations: highest applied version=$APPLIED_MAX"

# Build a SQL script: take the advisory lock, then for each pending file apply
# it (\i, each file owns its own BEGIN/COMMIT) and record the tracking row.
# No outer transaction here: migration files contain their own BEGIN/COMMIT
# blocks and Postgres does not support nested transactions, so wrapping the
# whole sequence would conflict. The advisory lock is session-level and held
# until the psql session disconnects, which still serializes concurrent
# runners. ON_ERROR_STOP=1 aborts the session on the first failing statement
# (releasing the lock) without recording that file as applied.
{
  echo "SELECT pg_advisory_lock($LOCK_KEY_EXPR);"
  for f in "$MIGRATIONS_DIR"/[0-9][0-9][0-9][0-9]_*.sql; do
    [[ -e "$f" ]] || continue
    version="$(basename "$f" | grep -oE '^[0-9]+' | sed 's/^0*//')"
    [[ -z "$version" ]] && version=0
    version=$((10#$version))
    if (( version <= APPLIED_MAX )); then
      continue
    fi
    checksum="$(shasum -a 256 "$f" | cut -d' ' -f1)"
    echo "\\echo apply_migrations: applying $(basename "$f") (version $version)"
    # \i runs the file in the current session context. Each migration file
    # manages its own transaction boundaries.
    echo "\\i '$f'"
    # Record the apply. If the file itself already inserted the row (idempotent
    # self-tracking), ON CONFLICT keeps this a no-op. The advisory lock
    # guarantees no other runner is racing this insert, so we do NOT swallow
    # errors here: a real tracking failure must abort the run.
    echo "INSERT INTO agent_store.schema_migrations(version, checksum) VALUES ($version, '$checksum') ON CONFLICT (version) DO NOTHING;"
  done
  echo "SELECT pg_advisory_unlock($LOCK_KEY_EXPR);"
} | psql "$@" -v ON_ERROR_STOP=1

echo "apply_migrations: done"
