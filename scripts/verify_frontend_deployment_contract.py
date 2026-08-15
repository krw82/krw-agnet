#!/usr/bin/env python3
"""Verify the single versioned frontend/agent deployment boundary.

This tool never crawls frontend source, migrations, or routes. The frontend
owns one canonical contract artifact and the sealed agent release records its
hash. Runtime behavior is verified by the normal DB ABI and deep-readiness
checks during deployment.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import sys
from typing import NoReturn


CONTRACT_ID = "krw.agent/frontend-deployment-contract/v1"
REQUIRED_FIELDS = {
    "schema_version",
    "contract_id",
    "agent_abi",
    "executor_type",
    "session_backend",
    "run_submission_contract",
    "final_projection_contract",
    "required_projection_fields",
    "optional_projection_fields",
    "visualization_failure_policy",
    "admission_policy",
}
CORE_PROJECTION_FIELDS = [
    "run_id",
    "answer_bundle_hash",
    "final_output_hash",
    "markdown",
]
OPTIONAL_PROJECTION_FIELDS = [
    "visualizations",
    "usage",
    "evidence_ledger_hash",
    "memory_revision",
    "memory_frontier_hash",
]


def fail(code: str) -> NoReturn:
    print(code, file=sys.stderr)
    raise SystemExit(1)


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        allow_nan=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def require_exact_string(value: dict[str, object], key: str, expected: str) -> None:
    if value.get(key) != expected:
        fail(f"{key}_invalid")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--contract", type=Path, required=True)
    args = parser.parse_args()

    requested_path = args.contract.absolute()
    if requested_path.is_symlink() or not requested_path.is_file():
        fail("frontend_contract_missing_or_unsafe")
    path = requested_path.resolve(strict=True)
    try:
        raw = path.read_bytes()
        value = json.loads(raw)
    except (OSError, UnicodeError, json.JSONDecodeError):
        fail("frontend_contract_json_invalid")
    if not isinstance(value, dict):
        fail("frontend_contract_root_invalid")
    if not REQUIRED_FIELDS.issubset(value):
        fail("frontend_contract_fields_mismatch")
    if value.get("schema_version") != 1:
        fail("schema_version_invalid")

    require_exact_string(value, "contract_id", CONTRACT_ID)
    require_exact_string(value, "agent_abi", "agent_v1_v7")
    require_exact_string(value, "executor_type", "rust_agent")
    require_exact_string(value, "session_backend", "rust_agent")
    require_exact_string(value, "run_submission_contract", "agent-v1-run-request/v1")
    require_exact_string(value, "final_projection_contract", "agent-final-projection/v1")
    require_exact_string(
        value,
        "visualization_failure_policy",
        "omit_artifact_keep_answer",
    )
    require_exact_string(
        value,
        "admission_policy",
        "open_after_exact_release_heartbeat",
    )

    if value.get("required_projection_fields") != CORE_PROJECTION_FIELDS:
        fail("required_projection_fields_mismatch")
    optional_fields = value.get("optional_projection_fields")
    if (
        not isinstance(optional_fields, list)
        or any(not isinstance(item, str) or not item for item in optional_fields)
        or len(set(optional_fields)) != len(optional_fields)
        or not set(OPTIONAL_PROJECTION_FIELDS).issubset(optional_fields)
        or set(CORE_PROJECTION_FIELDS).intersection(optional_fields)
    ):
        fail("optional_projection_fields_mismatch")

    digest = hashlib.sha256(canonical_bytes(value)).hexdigest()
    print(
        json.dumps(
            {
                "status": "ready",
                "contract_id": CONTRACT_ID,
                "contract_hash": f"sha256:{digest}",
            },
            sort_keys=True,
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
