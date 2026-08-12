#!/usr/bin/env python3
"""Run sequential same-room follow-up chains against an existing Gateway.

This runner never starts or rebuilds the local stack. Independent chains may
run in parallel, but every step in one chain is strictly serial and uses the
session ID returned by the preceding request (or the explicitly retained
initial session for ``resume_initial``). The selected provider is an operator
lane label (``KRW_AGENT_PROVIDER`` or ``--provider``); the Gateway itself
still owns the immutable descriptor and never accepts a provider from the
question payload.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path
from typing import Any

from release_provider import PROVIDER_MODELS, validate_public_descriptor

from run_live_quality_matrix import (
    GatewayProblem,
    action_trace_block,
    gateway_json,
    normalized_gateway_url,
    now_utc,
    private_report_dir,
    validate_status,
    validate_submit,
    validate_terminal_trace,
    write_private_json,
    write_private_text,
)

class RunnerError(RuntimeError):
    pass


def parse_args() -> argparse.Namespace:
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--corpus",
        type=Path,
        default=root / "fixtures/live-quality/v1/session-followup-v1.json",
    )
    parser.add_argument("--chain", action="append", default=[])
    parser.add_argument("--parallelism", type=int, default=1)
    parser.add_argument(
        "--gateway-url",
        default=os.environ.get("KRW_AGENT_GATEWAY_URL", "http://127.0.0.1:4318/v1/agent"),
    )
    parser.add_argument(
        "--provider",
        choices=tuple(sorted(PROVIDER_MODELS)),
        default=os.environ.get("KRW_AGENT_PROVIDER", "glm"),
        help="provider lane already loaded by the Gateway (default: KRW_AGENT_PROVIDER or glm)",
    )
    parser.add_argument("--timeout-seconds", type=int, default=720)
    parser.add_argument("--poll-seconds", type=float, default=2.0)
    parser.add_argument("--report-dir", type=Path, default=None)
    parser.add_argument("--dry-run", action="store_true")
    return parser.parse_args()


def load_corpus(path: Path) -> list[dict[str, Any]]:
    try:
        corpus = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise RunnerError("follow-up corpus is not valid UTF-8 JSON") from exc
    if not isinstance(corpus, dict) or set(corpus) != {"schema_version", "suite_id", "suite_version", "chains"}:
        raise RunnerError("follow-up corpus has an unexpected shape")
    if corpus["schema_version"] != 1 or not isinstance(corpus["chains"], list):
        raise RunnerError("follow-up corpus schema is unsupported")
    chains: list[dict[str, Any]] = []
    seen: set[str] = set()
    for chain in corpus["chains"]:
        if not isinstance(chain, dict) or set(chain) != {"chain_id", "steps"}:
            raise RunnerError("follow-up chain has an unexpected shape")
        chain_id = chain["chain_id"]
        steps = chain["steps"]
        if not isinstance(chain_id, str) or not chain_id or chain_id in seen:
            raise RunnerError("follow-up chain_id is invalid or duplicated")
        if not isinstance(steps, list) or not steps:
            raise RunnerError(f"follow-up chain has no steps: {chain_id}")
        seen.add(chain_id)
        previous_sessions = 0
        for index, step in enumerate(steps):
            if not isinstance(step, dict) or set(step) != {
                "step_id", "session_action", "ticker", "question", "evaluation_focus"
            }:
                raise RunnerError(f"follow-up step shape is invalid: {chain_id}")
            if not isinstance(step["step_id"], str) or not step["step_id"]:
                raise RunnerError(f"follow-up step_id is invalid: {chain_id}")
            action = step["session_action"]
            if action not in {"new", "continue", "resume_initial"}:
                raise RunnerError(f"unknown session_action: {action}")
            if index == 0 and action != "new":
                raise RunnerError(f"first step must be new: {chain_id}")
            if action == "continue" and previous_sessions == 0:
                raise RunnerError(f"continue has no current session: {chain_id}")
            if action == "resume_initial" and previous_sessions == 0:
                raise RunnerError(f"resume_initial has no initial session: {chain_id}")
            if not isinstance(step["ticker"], str) or not step["ticker"].isupper():
                raise RunnerError(f"ticker is invalid: {chain_id}")
            if not isinstance(step["question"], str) or not step["question"] or "\x00" in step["question"]:
                raise RunnerError(f"question is invalid: {chain_id}")
            if not isinstance(step["evaluation_focus"], list) or not step["evaluation_focus"]:
                raise RunnerError(f"evaluation_focus is invalid: {chain_id}")
            if action == "new":
                previous_sessions += 1
        chains.append(chain)
    if not chains:
        raise RunnerError("follow-up corpus has no chains")
    return chains


def selected_chains(chains: list[dict[str, Any]], requested: list[str]) -> list[dict[str, Any]]:
    if not requested:
        return chains
    by_id = {chain["chain_id"]: chain for chain in chains}
    if len(set(requested)) != len(requested) or any(value not in by_id for value in requested):
        raise RunnerError("unknown or duplicated --chain")
    return [by_id[value] for value in requested]


def planned_chain(chain: dict[str, Any]) -> dict[str, Any]:
    session = "<returned-session-id>"
    initial = "<initial-session-id>"
    return {
        "chain_id": chain["chain_id"],
        "status": "planned",
        "steps": [
            {
                "step_id": step["step_id"],
                "session_action": step["session_action"],
                "request_path": "/v1/agent/runs"
                if step["session_action"] == "new"
                else f"/v1/agent/sessions/{initial if step['session_action'] == 'resume_initial' else session}/runs",
                "session_id_source": "response.session_id",
                "question_original": step["question"],
                "evaluation_focus": step["evaluation_focus"],
            }
            for step in chain["steps"]
        ],
    }


def run_chain(
    chain: dict[str, Any],
    gateway_url: str,
    token: str,
    timeout_seconds: int,
    poll_seconds: float,
    report_root: Path,
) -> dict[str, Any]:
    chain_dir = report_root / "chains" / chain["chain_id"]
    # `private_report_dir` creates the root and its dry-run `cases` directory,
    # but live follow-up reports use a separate `chains/<id>` tree. Create the
    # exact bounded parent path here with the same private mode.
    chain_dir.mkdir(parents=True, mode=0o700)
    results: list[dict[str, Any]] = []
    current_session: str | None = None
    initial_session: str | None = None
    try:
        for step in chain["steps"]:
            action = step["session_action"]
            if action == "new":
                path = f"{gateway_url}/runs"
            else:
                session = initial_session if action == "resume_initial" else current_session
                if session is None:
                    raise GatewayProblem("session_reference_missing")
                path = f"{gateway_url}/sessions/{session}/runs"
            started = time.monotonic()
            submitted = gateway_json(
                "POST",
                path,
                token,
                {
                    "schema_version": 1,
                    "question": step["question"],
                    "ticker": step["ticker"],
                },
            )
            session_id, run_id, _ = validate_submit(submitted)
            if action == "new" and initial_session is None:
                initial_session = session_id
            current_session = session_id
            terminal: dict[str, Any] | None = None
            deadline = time.monotonic() + timeout_seconds
            while time.monotonic() < deadline:
                time.sleep(poll_seconds)
                status = gateway_json("GET", f"{gateway_url}/runs/{run_id}", token)
                _, _, state, _, _, _, _ = validate_status(status, session_id, run_id)
                if state in {"final", "cancelled", "failed"}:
                    terminal = status
                    break
            if terminal is None:
                raise GatewayProblem("terminal_wait_timeout")
            _, _, state, answer, final_hash, usage, retry_message = validate_status(
                terminal, session_id, run_id
            )
            trace = None
            action_trace = action_trace_block(None, reason="terminal_trace_unavailable")
            try:
                trace = gateway_json("GET", f"{gateway_url}/runs/{run_id}/trace", token)
                action_trace = action_trace_block(
                    validate_terminal_trace(trace, session_id, run_id, state)
                )
            except GatewayProblem as exc:
                trace = {"error": str(exc)}
            result = {
                "step_id": step["step_id"],
                "session_action": action,
                "question_original": step["question"],
                "evaluation_focus": step["evaluation_focus"],
                "answer_markdown_original": answer,
                "execution_trace": {
                    "session_id": session_id,
                    "run_id": run_id,
                    "state": state,
                    "elapsed_seconds": round(time.monotonic() - started, 3),
                    "final_output_hash": final_hash,
                    "usage": usage,
                    "retry_message": retry_message,
                    "action_trace": action_trace,
                },
                "quality": {
                    "transport_verdict": "passed" if state == "final" else "failed",
                    "research_quality_verdict": "manual_review_required" if state == "final" else "not_assessable",
                    "context_correct": None,
                    "no_cross_session_leak": None,
                    "research_depth_preserved": None,
                    "evidence_regrounded": None,
                    "resume_after_interleaving": None,
                },
            }
            write_private_json(chain_dir / f"{step['step_id']}.json", {"submitted": submitted, "terminal": terminal, "trace": trace, "result": result})
            results.append(result)
    except GatewayProblem as exc:
        return {"chain_id": chain["chain_id"], "status": "transport_failed", "error": str(exc), "steps": results}
    return {
        "chain_id": chain["chain_id"],
        "status": "completed",
        "initial_session_id": initial_session,
        "final_session_id": current_session,
        "session_ids": [result["execution_trace"]["session_id"] for result in results],
        "steps": results,
    }


def main() -> int:
    args = parse_args()
    if not 1 <= args.parallelism <= 4 or not 30 <= args.timeout_seconds <= 1800 or not 0.2 <= args.poll_seconds <= 10:
        raise RunnerError("parallelism/timeout/poll bounds are invalid")
    chains = selected_chains(load_corpus(args.corpus), args.chain)
    report_root = private_report_dir(args.report_dir)
    if args.dry_run:
        report = {
            "schema_version": 1,
            "suite_id": "chat-session-followup-v1",
            "generated_at": now_utc(),
            "mode": "dry_run",
            "provider_id": args.provider,
            "physical_model": PROVIDER_MODELS[args.provider],
            "chains": [planned_chain(chain) for chain in chains],
        }
        write_private_json(report_root / "report.json", report)
        print(json.dumps(report, ensure_ascii=False, indent=2))
        return 0
    token = os.environ.get("KRW_AGENT_GATEWAY_TOKEN", "")
    if not token:
        raise RunnerError("KRW_AGENT_GATEWAY_TOKEN is required unless --dry-run is used")
    gateway_url = normalized_gateway_url(args.gateway_url)
    descriptor_path = os.environ.get("KRW_AGENT_RELEASE_DESCRIPTOR_PATH", "").strip()
    descriptor_hash = os.environ.get("KRW_AGENT_RELEASE_ARTIFACT_HASH", "").strip()
    release_set_hash = os.environ.get("KRW_AGENT_RELEASE_SET_HASH", "").strip()
    if not descriptor_path or not descriptor_hash or not release_set_hash:
        raise RunnerError("provider release pins are required for live follow-up runs")
    try:
        lane = validate_public_descriptor(
            Path(descriptor_path),
            args.provider,
            expected_artifact_hash=descriptor_hash,
            expected_release_set_hash=release_set_hash,
        )
    except (OSError, ValueError) as error:
        raise RunnerError(f"provider release lane is invalid: {error}") from error
    # Keep chains serial by design. The CLI intentionally defaults to one; a
    # future bounded executor may parallelize independent chains without ever
    # interleaving steps within one room.
    if args.parallelism != 1:
        raise RunnerError("live follow-up runner currently requires --parallelism 1")
    reports = [run_chain(chain, gateway_url, token, args.timeout_seconds, args.poll_seconds, report_root) for chain in chains]
    failed = sum(report.get("status") != "completed" for report in reports)
    report = {
        "schema_version": 1,
        "suite_id": "chat-session-followup-v1",
        "generated_at": now_utc(),
        "mode": "live_gateway",
        "provider_id": lane["provider_id"],
        "physical_model": lane["physical_model"],
        "release": {
            key: lane[key]
            for key in ("descriptor_artifact_hash", "release_set_hash", "entry_count")
            if key in lane
        },
        "chains": reports,
        "completed_chains": len(reports) - failed,
        "failed_chains": failed,
        "manual_review_required": True,
    }
    write_private_json(report_root / "report.json", report)
    print(
        json.dumps(
            {
                "report_dir": str(report_root),
                "chains": len(reports),
                "completed_chains": len(reports) - failed,
                "failed_chains": failed,
            },
            ensure_ascii=False,
        )
    )
    return 1 if failed else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RunnerError as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(2)
