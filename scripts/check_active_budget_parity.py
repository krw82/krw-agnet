#!/usr/bin/env python3
"""Check that shipped AgentSpecs resolve to identical local/prod envelopes.

The registry format is deliberately small and is parsed without adding a YAML
runtime dependency. Only active ``required_budget_profile`` entries are
compared; comments and registry IDs are ignored. A difference is a release
block because it silently changes research capacity between environments.
"""

from __future__ import annotations

import json
import pathlib
import re
import sys
from typing import Any


PROFILE_RE = re.compile(r"^\s+- profile_id:\s*([A-Za-z0-9_.-]+)\s*$")
REQUIRED_RE = re.compile(r"^\s+required_budget_profile:\s*([A-Za-z0-9_.-]+)\s*$")


def active_profiles(root: pathlib.Path) -> set[str]:
    result: set[str] = set()
    for path in sorted((root / "agents").glob("*/agent.yaml")):
        for line in path.read_text(encoding="utf-8").splitlines():
            match = REQUIRED_RE.match(line)
            if match:
                result.add(match.group(1))
    if not result:
        raise ValueError("no active required_budget_profile entries found")
    return result


def parse_scalar(value: str) -> str:
    return value.strip()


def load_profiles(path: pathlib.Path) -> dict[str, dict[str, str]]:
    profiles: dict[str, dict[str, str]] = {}
    current: str | None = None
    in_limits = False
    in_caps = False
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.rstrip()
        profile = PROFILE_RE.match(line)
        if profile:
            current = profile.group(1)
            profiles[current] = {}
            in_limits = False
            in_caps = False
            continue
        if current is None or not line.strip() or line.lstrip().startswith("#"):
            continue
        indent = len(line) - len(line.lstrip(" "))
        stripped = line.strip()
        if indent == 4 and stripped == "limits:":
            in_limits = True
            in_caps = False
            continue
        if not in_limits:
            continue
        if indent == 6 and stripped == "capability_call_limits:":
            in_caps = True
            continue
        if indent == 6 and ":" in stripped:
            key, value = stripped.split(":", 1)
            profiles[current][key.strip()] = parse_scalar(value)
            in_caps = False
            continue
        if indent == 8 and in_caps and ":" in stripped:
            key, value = stripped.split(":", 1)
            profiles[current][f"capability_call_limits.{key.strip()}"] = parse_scalar(value)
    return profiles


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent
    active = active_profiles(root)
    local = load_profiles(root / "deployments/local/budget-registry.yaml")
    prod = load_profiles(root / "deployments/prod/budget-registry.yaml")
    missing_local = sorted(active - local.keys())
    missing_prod = sorted(active - prod.keys())
    differences: dict[str, dict[str, Any]] = {}
    for profile in sorted(active):
        if profile not in local or profile not in prod:
            continue
        keys = set(local[profile]) | set(prod[profile])
        changed = {
            key: {"local": local[profile].get(key), "prod": prod[profile].get(key)}
            for key in sorted(keys)
            if local[profile].get(key) != prod[profile].get(key)
        }
        if changed:
            differences[profile] = changed
    report = {
        "status": "pass" if not missing_local and not missing_prod and not differences else "fail",
        "active_profiles": sorted(active),
        "missing_local": missing_local,
        "missing_prod": missing_prod,
        "differences": differences,
    }
    print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True))
    return 0 if report["status"] == "pass" else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError) as error:
        print(f"budget parity: {error}", file=sys.stderr)
        raise SystemExit(2) from error
