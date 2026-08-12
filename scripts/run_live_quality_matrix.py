#!/usr/bin/env python3
"""Run a bounded live research-quality matrix through an existing Gateway.

This runner deliberately *does not* start PostgreSQL, the capability sidecar,
TLS, agentd, or the Gateway.  Start that stack once with
``scripts/start_local_agent_gateway_stack.sh`` and reuse it for every case.
That makes a 6--10 case per-bucket quality pass measure model/research work
rather than repeated local-stack boot time.

By default it selects six cases from each ``short_``, ``normal_``, and
``complex_`` bucket in the Supabase-derived corpus and dispatches at most two
independent runs at once.  The resulting private report includes the original
question, raw final Markdown, terminal Gateway receipt, sanitized action
trace, elapsed time, and evaluation focus.  It deliberately reports content
quality as
``manual_review_required`` rather than pretending a transport check proves
grounding or investment-research quality.

Examples:

  # Validate selection and report shape without a running Gateway or token.
  ./scripts/run_live_quality_matrix.py --dry-run

  # Reuse the long-lived local stack; the token stays in the operator shell.
  KRW_AGENT_GATEWAY_TOKEN=... ./scripts/run_live_quality_matrix.py

  # Re-run selected cases only, with one worker for diagnosis.
  KRW_AGENT_GATEWAY_TOKEN=... ./scripts/run_live_quality_matrix.py \
    --case short_aapl_price_drop --case normal_aapl_mix_cost_cash \
    --parallelism 1
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import re
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import urlparse, urlunparse
from urllib.request import Request, urlopen

from release_provider import PROVIDER_MODELS, validate_public_descriptor


MAX_CORPUS_BYTES = 2 * 1024 * 1024
MAX_GATEWAY_RESPONSE_BYTES = 256 * 1024
MAX_ANSWER_BYTES = 64 * 1024
MAX_TERMINAL_TRACE_ACTIONS = 512
CASE_ID_RE = re.compile(r"^[a-z0-9][a-z0-9_-]{0,63}$")
TICKER_RE = re.compile(r"^[A-Z0-9][A-Z0-9.-]{0,31}$")
SESSION_ID_RE = re.compile(r"^ses_[A-Za-z0-9_-]{1,128}$")
RUN_ID_RE = re.compile(r"^run_[A-Za-z0-9_-]{1,128}$")
HASH_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
CAPABILITY_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$")
KNOWN_STATES = {"queued", "deferred", "active", "final", "cancelled", "failed"}
TERMINAL_STATES = {"final", "cancelled", "failed"}
ACTION_STAGES = {"begun", "observed", "accepted", "rejected", "ambiguous"}
DEFAULT_BUCKETS = ("short", "normal", "complex")


class RunnerError(RuntimeError):
    """Configuration or corpus error that is safe to show to the operator."""


class GatewayProblem(RuntimeError):
    """A bounded, non-sensitive Gateway failure category."""


@dataclass(frozen=True)
class QualityCase:
    case_id: str
    ticker: str
    question: str
    evaluation_focus: tuple[str, ...]
    session_group: str | None = None


def parse_args() -> argparse.Namespace:
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--corpus",
        type=Path,
        default=Path(
            os.environ.get(
                "KRW_LIVE_QUALITY_CORPUS",
                root / "fixtures/live-quality/v1/supabase-company-research-24-case-corpus.json",
            )
        ),
        help="quality corpus JSON (default: Supabase-derived 24-case corpus)",
    )
    parser.add_argument(
        "--case",
        action="append",
        default=[],
        help="one case ID to run; repeat to bypass balanced bucket selection",
    )
    parser.add_argument(
        "--per-bucket",
        type=int,
        default=int(os.environ.get("KRW_LIVE_QUALITY_PER_BUCKET", "6")),
        help="cases selected from each short/normal/complex bucket (1..10; default: 6)",
    )
    parser.add_argument(
        "--parallelism",
        type=int,
        default=int(os.environ.get("KRW_LIVE_QUALITY_PARALLELISM", "2")),
        help="independent Gateway runs in flight (1..4; default: 2)",
    )
    parser.add_argument(
        "--gateway-url",
        default=os.environ.get(
            "KRW_AGENT_GATEWAY_URL", "http://127.0.0.1:4318/v1/agent"
        ),
        help="existing Agent Gateway base URL",
    )
    parser.add_argument(
        "--provider",
        choices=tuple(sorted(PROVIDER_MODELS)),
        default=os.environ.get("KRW_AGENT_PROVIDER", "glm"),
        help="provider lane already loaded by the Gateway (default: KRW_AGENT_PROVIDER or glm)",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=int,
        default=int(os.environ.get("KRW_LIVE_QUALITY_TIMEOUT_SECONDS", "720")),
        help="terminal wait bound per case (30..1800; default: 720)",
    )
    parser.add_argument(
        "--poll-seconds",
        type=float,
        default=float(os.environ.get("KRW_LIVE_QUALITY_POLL_SECONDS", "2")),
        help="Gateway poll interval (0.2..10; default: 2)",
    )
    parser.add_argument(
        "--report-dir",
        type=Path,
        default=(
            Path(os.environ["KRW_LIVE_QUALITY_REPORT_DIR"])
            if "KRW_LIVE_QUALITY_REPORT_DIR" in os.environ
            else None
        ),
        help="new absolute private report directory; must not already exist",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        default=os.environ.get("KRW_LIVE_QUALITY_DRY_RUN", "0") == "1",
        help="select cases and emit a report without a Gateway request",
    )
    return parser.parse_args()


def validate_args(args: argparse.Namespace) -> None:
    if not 1 <= args.per_bucket <= 10:
        raise RunnerError("--per-bucket must be in 1..10")
    if not 1 <= args.parallelism <= 4:
        raise RunnerError("--parallelism must be in 1..4")
    if not 30 <= args.timeout_seconds <= 1800:
        raise RunnerError("--timeout-seconds must be in 30..1800")
    if not 0.2 <= args.poll_seconds <= 10:
        raise RunnerError("--poll-seconds must be in 0.2..10")
    if args.report_dir is not None and not args.report_dir.is_absolute():
        raise RunnerError("--report-dir must be an absolute path")


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise RunnerError(f"duplicate JSON key in corpus: {key}")
        value[key] = item
    return value


def load_cases(path: Path) -> tuple[dict[str, Any], list[QualityCase]]:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise RunnerError(f"cannot read corpus: {path}") from exc
    if len(raw) > MAX_CORPUS_BYTES:
        raise RunnerError("quality corpus exceeds the 2 MiB safety limit")
    try:
        corpus = json.loads(raw, object_pairs_hook=unique_object)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RunnerError("quality corpus must be valid UTF-8 JSON") from exc
    if not isinstance(corpus, dict) or set(corpus) != {
        "schema_version",
        "suite_id",
        "suite_version",
        "cases",
    }:
        raise RunnerError("quality corpus has an unexpected shape")
    if corpus["schema_version"] != 1 or not isinstance(corpus["cases"], list):
        raise RunnerError("quality corpus has an unsupported schema version")
    if not isinstance(corpus["suite_id"], str) or not corpus["suite_id"]:
        raise RunnerError("quality corpus suite_id is invalid")
    if not isinstance(corpus["suite_version"], int):
        raise RunnerError("quality corpus suite_version is invalid")

    cases: list[QualityCase] = []
    seen: set[str] = set()
    for item in corpus["cases"]:
        if not isinstance(item, dict) or not {
            "case_id",
            "ticker",
            "question",
            "evaluation_focus",
        }.issubset(item) or set(item) - {
            "case_id",
            "ticker",
            "question",
            "evaluation_focus",
            "session_group",
        }:
            raise RunnerError("quality corpus case has an unexpected shape")
        case_id = item["case_id"]
        ticker = item["ticker"]
        question = item["question"]
        focus = item["evaluation_focus"]
        if not isinstance(case_id, str) or not CASE_ID_RE.fullmatch(case_id):
            raise RunnerError("quality corpus case_id is invalid")
        if case_id in seen:
            raise RunnerError(f"quality corpus has duplicate case_id: {case_id}")
        seen.add(case_id)
        if not isinstance(ticker, str) or not TICKER_RE.fullmatch(ticker):
            raise RunnerError(f"quality corpus ticker is invalid: {case_id}")
        if (
            not isinstance(question, str)
            or not question
            or "\x00" in question
            or "\n" in question
            or len(question.encode("utf-8")) > 64 * 1024
        ):
            raise RunnerError(f"quality corpus question is invalid: {case_id}")
        if (
            not isinstance(focus, list)
            or not 1 <= len(focus) <= 8
            or any(
                not isinstance(value, str)
                or not value
                or len(value.encode("utf-8")) > 128
                for value in focus
            )
        ):
            raise RunnerError(f"quality corpus evaluation_focus is invalid: {case_id}")
        session_group = item.get("session_group")
        if session_group is not None and (
            not isinstance(session_group, str)
            or not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", session_group)
        ):
            raise RunnerError(f"quality corpus session_group is invalid: {case_id}")
        cases.append(QualityCase(case_id, ticker, question, tuple(focus), session_group))
    if not cases:
        raise RunnerError("quality corpus has no cases")
    return corpus, cases


def select_cases(cases: list[QualityCase], args: argparse.Namespace) -> list[QualityCase]:
    by_id = {case.case_id: case for case in cases}
    if args.case:
        requested = []
        for raw in args.case:
            requested.extend(value.strip() for value in raw.split(",") if value.strip())
        if not requested:
            raise RunnerError("--case must name at least one case")
        if len(set(requested)) != len(requested):
            raise RunnerError("--case contains a duplicate case ID")
        unknown = [case_id for case_id in requested if case_id not in by_id]
        if unknown:
            raise RunnerError(f"unknown quality case: {unknown[0]}")
        return [by_id[case_id] for case_id in requested]

    selected: list[QualityCase] = []
    for bucket in DEFAULT_BUCKETS:
        candidates = [case for case in cases if case.case_id.startswith(f"{bucket}_")]
        if len(candidates) < args.per_bucket:
            raise RunnerError(
                f"corpus has only {len(candidates)} {bucket} cases; "
                f"cannot select {args.per_bucket}"
            )
        selected.extend(candidates[: args.per_bucket])
    return selected


def normalized_gateway_url(raw: str) -> str:
    try:
        parsed = urlparse(raw)
    except ValueError as exc:
        raise RunnerError("--gateway-url is invalid") from exc
    if (
        parsed.scheme not in {"http", "https"}
        or not parsed.netloc
        or parsed.username is not None
        or parsed.password is not None
        or parsed.params
        or parsed.query
        or parsed.fragment
    ):
        raise RunnerError("--gateway-url must be an absolute http(s) URL without credentials")
    path = parsed.path.rstrip("/")
    if not path:
        raise RunnerError("--gateway-url must include the Agent Gateway API path")
    return urlunparse((parsed.scheme, parsed.netloc, path, "", "", ""))


def private_report_dir(requested: Path | None) -> Path:
    if requested is None:
        path = Path(tempfile.mkdtemp(prefix="krw-agent-live-quality-"))
    else:
        path = requested
        if path.exists():
            raise RunnerError("--report-dir must not already exist")
        path.mkdir(parents=True, mode=0o700)
    path.chmod(0o700)
    (path / "cases").mkdir(mode=0o700)
    return path


def write_private_text(path: Path, value: str) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
        handle.write(value)


def write_private_json(path: Path, value: Any) -> None:
    write_private_text(
        path,
        json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
    )


def require_gateway_token() -> str:
    token = os.environ.get("KRW_AGENT_GATEWAY_TOKEN", "")
    if not token or len(token.encode("utf-8")) > 16 * 1024 or "\r" in token or "\n" in token:
        raise RunnerError("KRW_AGENT_GATEWAY_TOKEN must be a non-empty single-line value")
    return token


def read_response(response: Any) -> dict[str, Any]:
    body = response.read(MAX_GATEWAY_RESPONSE_BYTES + 1)
    if len(body) > MAX_GATEWAY_RESPONSE_BYTES:
        raise GatewayProblem("response_too_large")
    try:
        value = json.loads(body, object_pairs_hook=unique_object)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GatewayProblem("response_invalid_json") from exc
    if not isinstance(value, dict):
        raise GatewayProblem("response_not_object")
    return value


def gateway_json(
    method: str,
    url: str,
    token: str,
    payload: dict[str, Any] | None = None,
) -> dict[str, Any]:
    body = None
    headers = {"Authorization": f"Bearer {token}", "Accept": "application/json"}
    if payload is not None:
        body = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        headers["Content-Type"] = "application/json"
    request = Request(url, data=body, headers=headers, method=method)
    try:
        with urlopen(request, timeout=15) as response:  # noqa: S310 - URL is operator supplied and validated.
            if not 200 <= response.status < 300:
                raise GatewayProblem(f"http_{response.status}")
            return read_response(response)
    except HTTPError as exc:
        raise GatewayProblem(f"http_{exc.code}") from exc
    except URLError as exc:
        raise GatewayProblem("transport_unavailable") from exc
    except TimeoutError as exc:
        raise GatewayProblem("transport_timeout") from exc


def validate_submit(value: dict[str, Any]) -> tuple[str, str, str]:
    if set(value) != {"schema_version", "session_id", "run_id", "state"}:
        raise GatewayProblem("submit_shape_invalid")
    session_id, run_id, state = value["session_id"], value["run_id"], value["state"]
    if (
        type(value["schema_version"]) is not int
        or value["schema_version"] != 1
        or not isinstance(session_id, str)
        or not SESSION_ID_RE.fullmatch(session_id)
        or not isinstance(run_id, str)
        or not RUN_ID_RE.fullmatch(run_id)
        or not isinstance(state, str)
        or state not in {"queued", "deferred", "active"}
    ):
        raise GatewayProblem("submit_identity_invalid")
    return session_id, run_id, state


def validate_status(
    value: dict[str, Any], expected_session_id: str, expected_run_id: str
) -> tuple[str, str, str, str | None, str | None, dict[str, int] | None, dict[str, Any] | None]:
    if set(value) != {
        "schema_version",
        "session_id",
        "run_id",
        "state",
        "final_output",
        "usage",
        "retry_message",
    }:
        raise GatewayProblem("status_shape_invalid")
    session_id, run_id, state = value["session_id"], value["run_id"], value["state"]
    if (
        type(value["schema_version"]) is not int
        or value["schema_version"] != 1
        or not isinstance(session_id, str)
        or not SESSION_ID_RE.fullmatch(session_id)
        or session_id != expected_session_id
        or run_id != expected_run_id
        or not isinstance(state, str)
        or state not in KNOWN_STATES
    ):
        raise GatewayProblem("status_identity_invalid")
    final_output = value["final_output"]
    usage = value["usage"]
    retry_message = value["retry_message"]
    if state == "failed":
        if final_output is not None:
            raise GatewayProblem("status_failed_output")
        failed_usage = None if usage is None else validate_usage(usage)
        return session_id, run_id, state, None, None, failed_usage, validate_retry_message(retry_message)
    if state != "final":
        if final_output is not None or usage is not None or retry_message is not None:
            raise GatewayProblem("status_non_final_payload")
        return session_id, run_id, state, None, None, None, None
    if not isinstance(final_output, dict) or set(final_output) != {"markdown", "final_output_hash"}:
        raise GatewayProblem("final_output_shape_invalid")
    markdown = final_output["markdown"]
    final_hash = final_output["final_output_hash"]
    if (
        not isinstance(markdown, str)
        or not markdown
        or "\x00" in markdown
        or len(markdown.encode("utf-8")) > MAX_ANSWER_BYTES
        or not isinstance(final_hash, str)
        or not HASH_RE.fullmatch(final_hash)
    ):
        raise GatewayProblem("final_output_invalid")
    if retry_message is not None:
        raise GatewayProblem("status_final_retry_message")
    return session_id, run_id, state, markdown, final_hash, validate_usage(usage), None


def validate_usage(value: Any) -> dict[str, Any]:
    required_fields = {
        "provider_turns",
        "capability_calls",
        "repairs",
        "input_tokens",
        "output_tokens",
        "total_tokens",
        "token_usage_status",
        "billable_tokens",
        "provider_total_ms",
        "capability_total_ms",
    }
    # The Gateway's credit-facing usage contract gained optional runtime
    # timing counters after this runner was first written. Keep the core
    # accounting fields strict, while accepting those additive counters so a
    # valid terminal response is not misclassified as a transport failure.
    optional_timing_fields = {
        "compact_total_ms",
        "provider_queue_wait_ms",
        "session_memory_total_ms",
        "market_preflight_ms",
        "prompt_build_total_ms",
        "checkpoint_total_ms",
    }
    allowed_fields = required_fields | optional_timing_fields
    if (
        not isinstance(value, dict)
        or not required_fields.issubset(value)
        or not set(value).issubset(allowed_fields)
    ):
        raise GatewayProblem("usage_shape_invalid")
    fields = set(value)
    numeric_fields = fields - {"token_usage_status"}
    if any(type(value[key]) is not int or value[key] < 0 for key in numeric_fields):
        raise GatewayProblem("usage_value_invalid")
    if value["total_tokens"] != value["input_tokens"] + value["output_tokens"]:
        raise GatewayProblem("usage_total_invalid")
    if value["token_usage_status"] == "complete":
        if value["billable_tokens"] != value["total_tokens"]:
            raise GatewayProblem("usage_complete_billable_invalid")
    elif value["token_usage_status"] == "output_only":
        if value["input_tokens"] != 0 or value["billable_tokens"] != value["output_tokens"]:
            raise GatewayProblem("usage_output_only_billable_invalid")
    else:
        raise GatewayProblem("usage_status_invalid")
    return {
        key: int(value[key]) if key != "token_usage_status" else value[key]
        for key in sorted(fields)
    }


def validate_retry_message(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != {"markdown", "category", "retry_recommended"}:
        raise GatewayProblem("retry_message_shape_invalid")
    markdown = value["markdown"]
    category = value["category"]
    retry_recommended = value["retry_recommended"]
    if (
        not isinstance(markdown, str)
        or not markdown
        or "\x00" in markdown
        or len(markdown.encode("utf-8")) > MAX_ANSWER_BYTES
        or category not in {
            "model_response",
            "data_connection",
            "processing_limit",
            "service_setup",
            "temporary_processing",
        }
        or retry_recommended is not True
    ):
        raise GatewayProblem("retry_message_invalid")
    return {"markdown": markdown, "category": category, "retry_recommended": True}


def validate_terminal_trace(
    value: dict[str, Any],
    expected_session_id: str,
    expected_run_id: str,
    expected_state: str,
) -> list[dict[str, str | None]]:
    if set(value) != {"schema_version", "session_id", "run_id", "state", "actions"}:
        raise GatewayProblem("terminal_trace_shape_invalid")
    session_id, run_id, state, actions = (
        value["session_id"],
        value["run_id"],
        value["state"],
        value["actions"],
    )
    if (
        type(value["schema_version"]) is not int
        or value["schema_version"] != 1
        or not isinstance(session_id, str)
        or not SESSION_ID_RE.fullmatch(session_id)
        or session_id != expected_session_id
        or run_id != expected_run_id
        or not isinstance(state, str)
        or state not in TERMINAL_STATES
        or state != expected_state
        or not isinstance(actions, list)
        or len(actions) > MAX_TERMINAL_TRACE_ACTIONS
    ):
        raise GatewayProblem("terminal_trace_identity_invalid")
    normalized: list[dict[str, str | None]] = []
    for action in actions:
        if not isinstance(action, dict) or set(action) != {
            "capability_id",
            "stage",
            "result_hash",
        }:
            raise GatewayProblem("terminal_trace_action_shape_invalid")
        capability_id, stage, result_hash = (
            action["capability_id"],
            action["stage"],
            action["result_hash"],
        )
        if (
            not isinstance(capability_id, str)
            or not CAPABILITY_ID_RE.fullmatch(capability_id)
            or not isinstance(stage, str)
            or stage not in ACTION_STAGES
            or (
                result_hash is not None
                and (not isinstance(result_hash, str) or not HASH_RE.fullmatch(result_hash))
            )
        ):
            raise GatewayProblem("terminal_trace_action_invalid")
        normalized.append(
            {
                "capability_id": capability_id,
                "stage": stage,
                "result_hash": result_hash,
            }
        )
    return normalized


def action_trace_block(
    actions: list[dict[str, str | None]] | None,
    *,
    reason: str | None = None,
) -> dict[str, Any]:
    """Keep only operator-safe action metadata, never private reasoning or results."""

    if actions is None:
        return {
            "status": "unavailable",
            "action_count": 0,
            "capability_sequence": [],
            "chain_capability_called": False,
            "actions": [],
            "private_reasoning": "not_collected",
            "reason": reason or "terminal_trace_unavailable",
        }
    capability_sequence = [str(action["capability_id"]) for action in actions]
    return {
        "status": "available",
        "action_count": len(actions),
        "capability_sequence": capability_sequence,
        "chain_capability_called": "ontology.chain" in capability_sequence,
        "actions": actions,
        "private_reasoning": "not_collected",
    }


def now_utc() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def review_block(
    case: QualityCase,
    state: str,
    answer: str | None,
    action_trace: dict[str, Any],
) -> dict[str, Any]:
    if state != "final" or not answer:
        return {
            "transport_verdict": "failed",
            "research_quality_verdict": "not_assessable",
            "execution_trace_verdict": (
                "available" if action_trace["status"] == "available" else "missing"
            ),
            "required_focus": list(case.evaluation_focus),
            "reason": "No final answer was available for qualitative review.",
        }
    return {
        "transport_verdict": "passed",
        "research_quality_verdict": "manual_review_required",
        "execution_trace_verdict": (
            "available" if action_trace["status"] == "available" else "missing"
        ),
        "required_focus": list(case.evaluation_focus),
        "review_prompt": (
            "Check whether every required focus is answered with appropriate "
            "filing/market grounding, whether causal language is separated "
            "from facts, and whether uncertainty is explicit."
        ),
        "reason": (
            "A final Gateway response proves delivery only. It does not expose "
            "private chain-of-thought or prove factual grounding."
        ),
    }


def planned_result(case: QualityCase) -> dict[str, Any]:
    return {
        "case_id": case.case_id,
        "ticker": case.ticker,
        "question_original": case.question,
        "evaluation_focus": list(case.evaluation_focus),
        "session_group": case.session_group,
        "status": "planned",
        "answer_markdown": None,
        "execution_trace": {
            "run_id": None,
            "session_id": None,
            "state": "not_dispatched",
            "elapsed_seconds": 0.0,
            "final_output_hash": None,
            "usage": None,
            "retry_message": None,
            "action_trace": {
                "status": "not_dispatched",
                "action_count": 0,
                "capability_sequence": [],
                "chain_capability_called": False,
                "actions": [],
                "private_reasoning": "not_collected",
            },
        },
        "quality": {
            "transport_verdict": "not_run",
            "research_quality_verdict": "not_assessable",
            "execution_trace_verdict": "not_run",
            "required_focus": list(case.evaluation_focus),
            "reason": "Dry run does not contact the Gateway.",
        },
    }


def run_case(
    case: QualityCase,
    gateway_url: str,
    token: str,
    timeout_seconds: int,
    poll_seconds: float,
    report_root: Path,
    session_id: str | None = None,
) -> dict[str, Any]:
    started_at = now_utc()
    started = time.monotonic()
    case_dir = report_root / "cases" / case.case_id
    case_dir.mkdir(mode=0o700)
    write_private_text(case_dir / "question.txt", case.question + "\n")
    submit_path = case_dir / "submit-response.json"
    status_path = case_dir / "terminal-response.json"
    trace_path = case_dir / "terminal-trace-response.json"
    try:
        if session_id is None:
            submit_url = f"{gateway_url}/runs"
        else:
            if not SESSION_ID_RE.fullmatch(session_id):
                raise GatewayProblem("session_identity_invalid")
            submit_url = f"{gateway_url}/sessions/{session_id}/runs"
        submitted = gateway_json(
            "POST",
            submit_url,
            token,
            {"schema_version": 1, "question": case.question, "ticker": case.ticker},
        )
        write_private_json(submit_path, submitted)
        submitted_session_id, run_id, _ = validate_submit(submitted)
        if session_id is not None and submitted_session_id != session_id:
            raise GatewayProblem("session_identity_mismatch")
        session_id = submitted_session_id
        deadline = time.monotonic() + timeout_seconds
        terminal: dict[str, Any] | None = None
        while time.monotonic() < deadline:
            time.sleep(poll_seconds)
            status = gateway_json("GET", f"{gateway_url}/runs/{run_id}", token)
            _, _, state, _, _, _, _ = validate_status(status, session_id, run_id)
            if state in {"final", "cancelled", "failed"}:
                terminal = status
                break
        if terminal is None:
            raise GatewayProblem("terminal_wait_timeout")
        write_private_json(status_path, terminal)
        session_id, run_id, state, answer, final_hash, usage, retry_message = validate_status(
            terminal, session_id, run_id
        )
        try:
            terminal_trace = gateway_json(
                "GET", f"{gateway_url}/runs/{run_id}/trace", token
            )
            write_private_json(trace_path, terminal_trace)
            action_trace = action_trace_block(
                validate_terminal_trace(terminal_trace, session_id, run_id, state)
            )
        except GatewayProblem as trace_error:
            action_trace = action_trace_block(None, reason=str(trace_error))
        elapsed = round(time.monotonic() - started, 3)
        result = {
            "case_id": case.case_id,
            "ticker": case.ticker,
            "question_original": case.question,
            "evaluation_focus": list(case.evaluation_focus),
            "session_group": case.session_group,
            "status": "completed" if state == "final" else "terminal_non_final",
            "answer_markdown": answer,
            "execution_trace": {
                "submitted_at": started_at,
                "run_id": run_id,
                "session_id": session_id,
                "state": state,
                "elapsed_seconds": elapsed,
                "final_output_hash": final_hash,
                "usage": usage,
                "retry_message": retry_message,
                "action_trace": action_trace,
            },
            "quality": review_block(case, state, answer, action_trace),
            "artifacts": {
                "question": f"cases/{case.case_id}/question.txt",
                "submit_response": f"cases/{case.case_id}/submit-response.json",
                "terminal_response": f"cases/{case.case_id}/terminal-response.json",
                "terminal_trace_response": (
                    f"cases/{case.case_id}/terminal-trace-response.json"
                    if trace_path.exists()
                    else None
                ),
            },
        }
    except GatewayProblem as exc:
        elapsed = round(time.monotonic() - started, 3)
        result = {
            "case_id": case.case_id,
            "ticker": case.ticker,
            "question_original": case.question,
            "evaluation_focus": list(case.evaluation_focus),
            "session_group": case.session_group,
            "status": "transport_failed",
            "answer_markdown": None,
            "execution_trace": {
                "submitted_at": started_at,
                "run_id": None,
                "session_id": None,
                "state": "unknown",
                "elapsed_seconds": elapsed,
                "final_output_hash": None,
                "usage": None,
                "retry_message": None,
                "action_trace": action_trace_block(None, reason="run_not_terminal"),
            },
            "quality": {
                "transport_verdict": "failed",
                "research_quality_verdict": "not_assessable",
                "execution_trace_verdict": "not_available",
                "required_focus": list(case.evaluation_focus),
                "reason": f"Gateway transport/protocol failure: {exc}",
            },
            "artifacts": {
                "question": f"cases/{case.case_id}/question.txt",
                "submit_response": (
                    f"cases/{case.case_id}/submit-response.json" if submit_path.exists() else None
                ),
                "terminal_response": (
                    f"cases/{case.case_id}/terminal-response.json" if status_path.exists() else None
                ),
                "terminal_trace_response": (
                    f"cases/{case.case_id}/terminal-trace-response.json"
                    if trace_path.exists()
                    else None
                ),
            },
        }
    write_private_json(case_dir / "result.json", result)
    return result


def main() -> int:
    args = parse_args()
    validate_args(args)
    corpus, all_cases = load_cases(args.corpus)
    selected = select_cases(all_cases, args)
    report_root = private_report_dir(args.report_dir)
    lane: dict[str, object] = {
        "provider_id": args.provider,
        "physical_model": PROVIDER_MODELS[args.provider],
    }
    if args.dry_run:
        results = [planned_result(case) for case in selected]
    else:
        gateway_url = normalized_gateway_url(args.gateway_url)
        token = require_gateway_token()
        descriptor_path = os.environ.get("KRW_AGENT_RELEASE_DESCRIPTOR_PATH", "").strip()
        descriptor_hash = os.environ.get("KRW_AGENT_RELEASE_ARTIFACT_HASH", "").strip()
        release_set_hash = os.environ.get("KRW_AGENT_RELEASE_SET_HASH", "").strip()
        if not descriptor_path or not descriptor_hash or not release_set_hash:
            raise RunnerError("provider release pins are required for live quality runs")
        try:
            lane = validate_public_descriptor(
                Path(descriptor_path),
                args.provider,
                expected_artifact_hash=descriptor_hash,
                expected_release_set_hash=release_set_hash,
            )
        except (OSError, ValueError) as error:
            raise RunnerError(f"provider release lane is invalid: {error}") from error
        results_by_id: dict[str, dict[str, Any]] = {}

        # Cases in one session_group are deliberately serialized so the
        # second question can use the exact session returned by the first.
        # Independent rooms remain parallel and therefore retain the normal
        # multi-user quality/throughput coverage.
        grouped: dict[str, list[QualityCase]] = {}
        for case in selected:
            grouped.setdefault(case.session_group or f"case:{case.case_id}", []).append(case)

        def run_group(group: list[QualityCase]) -> list[dict[str, Any]]:
            group_results: list[dict[str, Any]] = []
            session_id: str | None = None
            for item in group:
                result = run_case(
                    item,
                    gateway_url,
                    token,
                    args.timeout_seconds,
                    args.poll_seconds,
                    report_root,
                    session_id=session_id,
                )
                group_results.append(result)
                observed = result.get("execution_trace", {}).get("session_id")
                if isinstance(observed, str) and SESSION_ID_RE.fullmatch(observed):
                    session_id = observed
            return group_results

        with concurrent.futures.ThreadPoolExecutor(max_workers=args.parallelism) as executor:
            futures = {
                executor.submit(run_group, group): group
                for group in grouped.values()
            }
            for future in concurrent.futures.as_completed(futures):
                group = futures[future]
                try:
                    for result in future.result():
                        results_by_id[result["case_id"]] = result
                except Exception:
                    # A worker bug is kept content-free and cannot suppress
                    # other independent runs or the final report.
                    for case in group:
                        failed_result = {
                            **planned_result(case),
                            "status": "runner_failed",
                            "quality": {
                                "transport_verdict": "failed",
                                "research_quality_verdict": "not_assessable",
                                "required_focus": list(case.evaluation_focus),
                                "reason": "Runner worker failed before a usable Gateway receipt.",
                            },
                        }
                        failed_dir = report_root / "cases" / case.case_id
                        failed_dir.mkdir(mode=0o700, exist_ok=True)
                        question_path = failed_dir / "question.txt"
                        if not question_path.exists():
                            write_private_text(question_path, case.question + "\n")
                        result_path = failed_dir / "result.json"
                        if not result_path.exists():
                            write_private_json(result_path, failed_result)
                        results_by_id[case.case_id] = failed_result
        results = [results_by_id[case.case_id] for case in selected]

    completed = sum(item["status"] == "completed" for item in results)
    failed = sum(item["quality"]["transport_verdict"] == "failed" for item in results)
    report = {
        "schema_version": 1,
        "kind": "krw-agent-live-quality-matrix/v1",
        "generated_at": now_utc(),
        "mode": "dry_run" if args.dry_run else "live_gateway_reuse",
        "provider_id": lane["provider_id"],
        "physical_model": lane["physical_model"],
        "release": {
            key: lane[key]
            for key in ("descriptor_artifact_hash", "release_set_hash", "entry_count")
            if key in lane
        },
        "corpus": {
            "path": str(args.corpus.resolve()),
            "suite_id": corpus["suite_id"],
            "suite_version": corpus["suite_version"],
        },
        "selection": {
            "case_count": len(selected),
            "per_bucket": None if args.case else args.per_bucket,
            "parallelism": args.parallelism,
        },
        "summary": {
            "completed": completed,
            "transport_failed": failed,
            "manual_quality_reviews_required": completed,
        },
        "results": results,
    }
    write_private_json(report_root / "report.json", report)
    print(
        "live quality matrix: "
        f"cases={len(selected)} completed={completed} transport_failed={failed} "
        f"report={report_root}"
    )
    return 1 if failed else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RunnerError as exc:
        print(f"live quality matrix: {exc}", file=sys.stderr)
        raise SystemExit(2) from exc
