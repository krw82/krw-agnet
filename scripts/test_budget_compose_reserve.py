#!/usr/bin/env python3
"""Every entrypoint's budget profile must exceed the compose retry reserve.

The engine rejects a run at start unless the profile's max_output_tokens
exceeds effective_final_output_reserve_tokens + minimum_research_turn_tokens,
where the effective reserve is max(answer_policy.final_output_reserve_tokens,
2 x the largest compose-role max_output_tokens in the workflow) — see
crates/agent-image effective_final_output_reserve_tokens and run-engine
validation. A profile below that threshold fails EVERY run of the kind at
enqueue with `final output reservation exceeds output budget` (production
2026-08-19: scenario_sensitivity, idea_generation, wide_research, and
earnings_deep_dive all sat below 34816 after composer caps rose to 16384).

Parsed without a YAML dependency, mirroring check_active_budget_parity.py.
"""

from __future__ import annotations

import pathlib
import re
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
REGISTRIES = [
    ROOT / "deployments" / "prod" / "budget-registry.yaml",
    ROOT / "deployments" / "local" / "budget-registry.yaml",
]


def parse_registry_profiles(path: pathlib.Path) -> dict[str, int]:
    profiles: dict[str, int] = {}
    current: str | None = None
    for line in path.read_text(encoding="utf-8").splitlines():
        match = re.match(r"\s*- profile_id:\s*(\S+)$", line)
        if match:
            current = match.group(1)
            continue
        match = re.match(r"\s+max_output_tokens:\s*(\d+)\s*$", line)
        if match and current:
            profiles[current] = int(match.group(1))
            current = None
    if not profiles:
        raise ValueError(f"no budget profiles parsed from {path}")
    return profiles


def parse_agent(path: pathlib.Path) -> tuple[dict[str, int], dict[str, int], dict[str, list[str]], dict[str, dict[str, str]]]:
    policy: dict[str, int] = {}
    role_caps: dict[str, int] = {}
    workflow_compose_roles: dict[str, list[str]] = {}
    entrypoints: dict[str, dict[str, str]] = {}
    section: str | None = None
    current_role: str | None = None
    current_workflow: str | None = None
    current_entry: str | None = None
    for line in path.read_text(encoding="utf-8").splitlines():
        top = re.match(r"(\S[^:]*):\s*$", line)
        if top:
            name = top.group(1)
            section = name if name in {"answer_policy", "roles", "workflows", "entrypoints"} else None
            current_role = current_workflow = current_entry = None
            continue
        if section == "answer_policy":
            match = re.match(r"  (final_output_reserve_tokens|minimum_research_turn_tokens):\s*(\d+)\s*$", line)
            if match:
                policy[match.group(1)] = int(match.group(2))
        elif section == "roles":
            match = re.match(r"  - id:\s*(\S+)\s*$", line)
            if match:
                current_role = match.group(1)
                continue
            if current_role:
                match = re.search(r"max_output_tokens:\s*(\d+)", line)
                if match and current_role not in role_caps:
                    role_caps[current_role] = int(match.group(1))
        elif section == "workflows":
            match = re.match(r"  - id:\s*(\S+)\s*$", line)
            if match:
                current_workflow = match.group(1)
                workflow_compose_roles[current_workflow] = []
                continue
            if current_workflow:
                match = re.match(r"\s+- \{[^}]*kind: compose[^}]*\}\s*$", line)
                if match:
                    role = re.search(r"role_id:\s*([A-Za-z0-9_]+)", match.group(0))
                    if role:
                        workflow_compose_roles[current_workflow].append(role.group(1))
        elif section == "entrypoints":
            match = re.match(r"  ([A-Za-z0-9_]+):\s*$", line)
            if match:
                current_entry = match.group(1)
                entrypoints[current_entry] = {}
                continue
            if current_entry:
                for key in ("workflow", "required_budget_profile"):
                    match = re.match(rf"    {key}:\s*(\S+)\s*$", line)
                    if match:
                        entrypoints[current_entry][key] = match.group(1)
    return policy, role_caps, workflow_compose_roles, entrypoints


class ComposeReserveThresholdTest(unittest.TestCase):
    def test_every_compose_entrypoint_exceeds_the_reserve_threshold(self) -> None:
        checked = 0
        for registry_path in REGISTRIES:
            profiles = parse_registry_profiles(registry_path)
            for agent_path in sorted((ROOT / "agents").glob("*/agent.yaml")):
                policy, role_caps, workflows, entrypoints = parse_agent(agent_path)
                base_reserve = policy.get("final_output_reserve_tokens")
                min_turn = policy.get("minimum_research_turn_tokens", 0)
                if base_reserve is None:
                    continue
                for name, entry in entrypoints.items():
                    compose_roles = workflows.get(entry.get("workflow", ""), [])
                    if not compose_roles:
                        continue
                    caps = [role_caps[role] for role in compose_roles if role in role_caps]
                    self.assertEqual(
                        len(caps), len(compose_roles),
                        f"{agent_path.parent.name}/{name}: compose role without a parsed cap",
                    )
                    threshold = max(base_reserve, max(caps) * 2) + min_turn
                    profile_id = entry.get("required_budget_profile")
                    self.assertIn(
                        profile_id, profiles,
                        f"{registry_path.name}: entrypoint {agent_path.parent.name}/{name} "
                        f"needs profile {profile_id}",
                    )
                    budget = profiles[profile_id]
                    checked += 1
                    self.assertGreater(
                        budget, threshold,
                        f"{registry_path.name} {agent_path.parent.name}/{name}: profile "
                        f"{profile_id} max_output_tokens={budget} must exceed the compose "
                        f"retry reserve threshold {threshold} or every run dies at enqueue "
                        f"('final output reservation exceeds output budget')",
                    )
        self.assertGreaterEqual(checked, 5, "parser went vacuous: too few entrypoints checked")


if __name__ == "__main__":
    sys.exit(unittest.main(verbosity=2))
