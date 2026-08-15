#!/usr/bin/env python3
"""Read-only compatibility preflight for the product chat -> agent_v1 boundary.

The product repository is deliberately not imported or modified.  This keeps
the existing routing/provider checks while also pinning ownership of the
optional visualization fields to the Agent migration.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys


EXPECTED_PROJECTION_KEYS = (
    "run_id",
    "answer_bundle_hash",
    "final_output_hash",
    "markdown",
    "visualizations",
    "usage",
    "evidence_ledger_hash",
    "memory_revision",
    "memory_frontier_hash",
)


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
            "provider_models_have_current_pins",
            all(model in sql for model in ("glm-5.3", "deepseek-v4-flash")),
        ),
        (
            "live_route_uses_rust_agent",
            ("executorType: \"rust_agent\"" in route or 'agent_backend: "rust_agent"' in route)
            and 'executorType: "claude_agent"' not in route,
        ),
        (
            "rust_agent_session_cutover",
            "agent_backend" in sql and "rust_agent" in sql,
        ),
    ]

    agent_migration = Path("migrations/0021_final_projection_contract.sql")
    front_migration = front_root / "supabase/migrations/20260815130000_agent_v1_final_visualizations.sql"
    agent_sql = agent_migration.read_text(encoding="utf-8") if agent_migration.is_file() else ""
    front_sql = front_migration.read_text(encoding="utf-8") if front_migration.is_file() else ""
    return_object = agent_sql.split("RETURN jsonb_build_object(", 1)
    return_body = return_object[1].split(");", 1)[0] if len(return_object) == 2 else ""
    checks.extend(
        [
            (
                "agent_projection_owns_function",
                bool(
                    re.search(
                        r"create\s+or\s+replace\s+function\s+agent_v1\.read_final_projection",
                        agent_sql,
                        re.IGNORECASE,
                    )
                ),
            ),
            (
                "frontend_does_not_redefine_agent_projection",
                not bool(
                    re.search(
                        r"create\s+or\s+replace\s+function\s+agent_v1\.read_final_projection",
                        front_sql,
                        re.IGNORECASE,
                    )
                ),
            ),
            (
                "agent_projection_has_exact_fields",
                bool(return_body)
                and all(f"'{key}'" in return_body for key in EXPECTED_PROJECTION_KEYS),
            ),
            (
                "frontend_consumes_agent_projection",
                "agent_v1.read_final_projection" in front_sql,
            ),
            (
                "frontend_visualization_projection_is_optional",
                "visualization_omitted_count" in front_sql,
            ),
            (
                "frontend_visualization_shape_is_bounded",
                all(
                    token in front_sql
                    for token in (
                        "artifact_format",
                        "schema_version",
                        "jsonb_array_length(v_artifact->'views')",
                        "v_artifact ?| array['html'",
                    )
                ),
            ),
        ]
    )

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
    sys.exit(main())
