#!/usr/bin/env python3
"""Read-only compatibility preflight for the product chat -> agent_v1 boundary.

The product repository is deliberately not imported or modified. This check is
run before a deployment cutover so a stale model allow-list or legacy executor
route cannot look like a successful krw-agent integration.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


def collect_text(root: Path, suffix: str) -> str:
    chunks: list[str] = []
    for path in sorted(root.rglob(f"*{suffix}")):
        if path.is_file():
            chunks.append(f"\n-- {path}\n{path.read_text(encoding='utf-8')}\n")
    return "\n".join(chunks)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--front-root", type=Path, required=True)
    parser.add_argument("--sql-dir", type=Path)
    args = parser.parse_args()

    front_root = args.front_root.resolve()
    sql_dir = (args.sql_dir or front_root / "supabase" / "migrations").resolve()
    errors: list[str] = []

    if not front_root.is_dir():
        errors.append(f"front_root_missing:{front_root}")
    if not sql_dir.is_dir():
        errors.append(f"sql_dir_missing:{sql_dir}")
    if errors:
        for error in errors:
            print(f"FAIL {error}")
        return 1

    sql = collect_text(sql_dir, ".sql")
    route_path = front_root / "src" / "app" / "api" / "chat" / "run" / "route.ts"
    route = route_path.read_text(encoding="utf-8") if route_path.is_file() else ""

    checks: list[tuple[str, bool]] = [
        (
            "identity_tuple_mapping",
            all(
                token in sql
                for token in (
                    "v_agent_request->>'run_id'",
                    "v_agent_request->>'session_id'",
                    "v_agent_request->>'principal_id'",
                    "v_agent_request#>>'{immutable_snapshot,request,run_id}'",
                    "v_agent_request#>>'{immutable_snapshot,request,session_id}'",
                    "v_agent_request#>>'{immutable_snapshot,request,principal_id}'",
                )
            ),
        ),
        (
            "service_owned_idempotent_enqueue",
            "enqueue_agent_v1_product_run" in sql
            and "already_enqueued" in sql
            and "request_id" in sql,
        ),
        (
            "deduplicated_terminal_projection",
            "dedupe_key" in sql and "answer.committed" in sql,
        ),
        (
            "provider_models_allowed",
            all(
                model in sql
                for model in ("glm-5.3", "deepseek-v4-flash")
            )
            and not re.search(r"\^deepseek-", sql),
        ),
        (
            "live_route_uses_rust_agent",
            "executorType: \"rust_agent\"" in route
            and "executorType: \"claude_agent\"" not in route,
        ),
        (
            "legacy_room_cutover_policy",
            "bootstrap" in route.lower() or "new room" in route.lower(),
        ),
    ]

    for name, passed in checks:
        if passed:
            print(f"PASS {name}")
        else:
            print(f"FAIL {name}")
            errors.append(name)
    if errors:
        print("product_projection_compatibility=blocked")
        return 1
    print("product_projection_compatibility=ready")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
