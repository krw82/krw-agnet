#!/usr/bin/env bash
set -euo pipefail

krw_repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
krw_pg_tmp=$(mktemp -d /tmp/krw-session-memory-test.XXXXXX)
krw_pg_port=$((54000 + $$ % 1000))

cleanup_krw_session_memory_postgres() {
  /opt/homebrew/bin/pg_ctl -D "$krw_pg_tmp/data" -m immediate stop >/dev/null 2>&1 || true
  case "$krw_pg_tmp" in
    /tmp/krw-session-memory-test.*) rm -r -- "$krw_pg_tmp" ;;
  esac
}
trap cleanup_krw_session_memory_postgres EXIT

for krw_pg_binary in initdb pg_ctl psql; do
  if [[ ! -x "/opt/homebrew/bin/$krw_pg_binary" ]]; then
    printf 'missing PostgreSQL binary: %s\n' "$krw_pg_binary" >&2
    exit 1
  fi
done

/opt/homebrew/bin/initdb -A trust -D "$krw_pg_tmp/data" >"$krw_pg_tmp/initdb.log"
/opt/homebrew/bin/pg_ctl \
  -D "$krw_pg_tmp/data" \
  -l "$krw_pg_tmp/postgres.log" \
  -o "-F -k $krw_pg_tmp -p $krw_pg_port -c listen_addresses=''" \
  -w start >/dev/null

for krw_migration in \
  migrations/0001_agent_v1.sql \
  migrations/0002_action_finalization.sql \
  migrations/0003_bounded_child.sql \
  migrations/0004_session_memory.sql \
  migrations/0005_session_memory_snapshot.sql
do
  /opt/homebrew/bin/psql -X -v ON_ERROR_STOP=1 \
    -h "$krw_pg_tmp" -p "$krw_pg_port" -d postgres \
    -f "$krw_repo_dir/$krw_migration" >/dev/null
done

/opt/homebrew/bin/psql -X -v ON_ERROR_STOP=1 \
  -h "$krw_pg_tmp" -p "$krw_pg_port" -d postgres \
  -f "$krw_repo_dir/scripts/test-session-memory-postgres.sql" >/dev/null

printf 'session-memory PostgreSQL integration: ok\n'
