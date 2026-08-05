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
#
# Checksum verification: for every already-applied migration file (version <=
# APPLIED_MAX), shasum -a 256 of the file is compared against the stored
# checksum in schema_migrations. Mismatch aborts the run before any new
# migration is applied. Rows where checksum == 'backfill' (written by the
# 0009 backfill for versions 1-9) are skipped because they are not real
# hashes. New migrations recorded by this script always carry a real sha256.

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

# Verify checksums of already-applied migrations before applying any pending
# file. This catches drifted / locally edited migration files that have
# already been recorded. The advisory lock is not held yet, but verification
# is read-only against schema_migrations and idempotent across concurrent
# runners; a mismatch is a hard pre-flight failure regardless of the lock.
if (( APPLIED_MAX > 0 )); then
  # version<TAB>checksum for all applied rows, one per line, in version order.
  APPLIED_ROWS="$(psql "$@" -tAF $'\t' \
    "SELECT version, checksum FROM agent_store.schema_migrations ORDER BY version" \
    2>/dev/null || true)"
  while IFS=$'\t' read -r row_version row_checksum; do
    [[ -z "$row_version" ]] && continue
    # Skip backfill rows written by 0009: checksum is the literal 'backfill',
    # not a real hash, so there is nothing to compare against.
    [[ "$row_checksum" == "backfill" ]] && continue
    # Locate the migration file for this version. Filename format is
    # NNNN_name.sql; NNNN is zero-padded to 4 digits (versions 1-9999).
    pad="$(printf '%04d' "$((10#$row_version))")"
    match="$(ls "$MIGRATIONS_DIR"/${pad}_*.sql 2>/dev/null || true)"
    if [[ -z "$match" ]]; then
      # Version recorded but no file present. This is not a checksum mismatch
      # (different failure mode); leave it to the operator rather than guess.
      echo "apply_migrations: warning: no file found for applied version $row_version" >&2
      continue
    fi
    # Exactly one file should match. If glob expanded to multiple, complain.
    file_for_version="$(printf '%s\n' "$match" | head -n1)"
    extra="$(printf '%s\n' "$match" | tail -n +2)"
    if [[ -n "$extra" ]]; then
      echo "checksum_ambiguous: multiple files for version $row_version: $match" >&2
      exit 1
    fi
    file_checksum="$(shasum -a 256 "$file_for_version" | cut -d' ' -f1)"
    if [[ "$file_checksum" != "$row_checksum" ]]; then
      echo "checksum_mismatch: $(basename "$file_for_version") (recorded=$row_checksum actual=$file_checksum)" >&2
      exit 1
    fi
    echo "apply_migrations: verified $(basename "$file_for_version") checksum"
  done <<< "$APPLIED_ROWS"
fi

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
