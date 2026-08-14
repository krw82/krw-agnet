#!/usr/bin/env python3
"""Run a bounded, model-free audit against the local MCP gateways.

This script checks transport and retrieval semantics only.  It does not call a
provider, ask an LLM to synthesize an answer, or decide whether an investment
insight is sensible.  An empty result is an observed data state, not a test
failure; malformed transport, identity drift, and an error payload that is
silently shaped as an empty result are failures.

The script is deliberately opt-in and is not part of the request path.  It can
therefore be used after a local release/gateway restart without adding a new
runtime failure branch.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import ssl
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any


CONTRACT_ID = "krw-agent/mcp-tool-session-stateless/v1"
EXPECTED_ENDPOINTS = ("ontology", "feed", "filings", "guru")
EXPECTED_REUSE = {
    "ontology": "attested-stateless-v1",
    "feed": "run-scoped",
    "filings": "run-scoped",
    "guru": "attested-stateless-v1",
}
TOKEN_ENV = {"feed": "KRW_FEED_MCP_TOKEN", "filings": "FILINGS_MCP_AUTH_TOKEN"}
ENV_KEY_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
DYNAMIC_ENV_SOURCE_BASENAMES = {".env", ".env.mac-worker.production"}


class AuditError(RuntimeError):
    """A transport or contract failure, not a data absence."""


@dataclass(frozen=True)
class Endpoint:
    name: str
    port: int
    reuse: str
    service: str
    protocol_version: str
    build_id: str
    schema_hash: str
    release_hash: str


@dataclass(frozen=True)
class TokenResolution:
    """A secret value plus the non-secret way the audit obtained it."""

    value: str | None
    source: str


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--operator-root",
        default=os.environ.get("KRW_AGENT_OPERATOR_ROOT", str(Path.home() / "krw-agnet-prod")),
        help="Operator root containing runtime/local-mcp-gateway-contract.json and tls/ca.pem.",
    )
    parser.add_argument("--ticker", default="AAPL", help="Ticker for the bounded read probes.")
    parser.add_argument(
        "--strict-auth",
        action="store_true",
        help="Fail when a Feed/Filings token is not available instead of reporting it as skipped.",
    )
    parser.add_argument(
        "--front-env-file",
        help="Optional frontend env file whose Feed/Filings credentials override the operator env for this audit.",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit the bounded report as JSON (the default is a compact human-readable summary).",
    )
    return parser.parse_args()


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise AuditError(f"cannot read JSON contract: {path.name}") from exc
    if not isinstance(value, dict):
        raise AuditError(f"JSON contract is not an object: {path.name}")
    return value


def env_file_value(path: Path, key: str) -> str | None:
    """Read one operator-owned env value without ever printing it."""

    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return None
    resolved: str | None = None
    for line in lines:
        stripped = line.strip()
        if not stripped or stripped.startswith("#") or "=" not in stripped:
            continue
        if stripped.startswith("export "):
            stripped = stripped[len("export ") :].lstrip()
        candidate, value = stripped.split("=", 1)
        if candidate.strip() != key:
            continue
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
            value = value[1:-1]
        resolved = value or None
    return resolved


def dynamic_read_env_reference(value: str) -> tuple[Path, str] | None:
    """Parse only the operator-generated ``$(read_env_value PATH KEY)`` form.

    The audit never sources an env file or runs shell interpolation.  This
    small parser exists because the production operator env intentionally uses
    that one generated form to refresh a credential at daemon start.  Anything
    else remains unresolved instead of being evaluated.
    """

    if not value.startswith("$(") or not value.endswith(")"):
        return None
    try:
        parts = shlex.split(value[2:-1].strip(), posix=True)
    except ValueError:
        return None
    if len(parts) != 3 or parts[0] != "read_env_value" or not ENV_KEY_RE.fullmatch(parts[2]):
        return None
    path = Path(parts[1]).expanduser()
    if not path.is_absolute():
        return None
    return path, parts[2]


def resolve_operator_env_value(path: Path, key: str) -> TokenResolution:
    """Resolve one static or generated operator value without shell execution."""

    raw = env_file_value(path, key)
    if not raw:
        return TokenResolution(None, "operator_missing")
    reference = dynamic_read_env_reference(raw)
    if reference is None:
        return TokenResolution(raw, "operator_static")
    source_path, source_key = reference
    # A generated runtime envelope may point only to the two known front env
    # files.  Do not turn this audit into a general shell/env reader.
    if (
        source_path.name not in DYNAMIC_ENV_SOURCE_BASENAMES
        or source_path.is_symlink()
        or not source_path.is_file()
    ):
        return TokenResolution(None, "operator_dynamic_unresolved")
    return TokenResolution(env_file_value(source_path, source_key), "operator_dynamic")


def resolve_feed_token_from_env(path: Path, source: str) -> TokenResolution:
    """Resolve the Feed aliases deterministically without exposing either value."""

    canonical = env_file_value(path, "KRW_FEED_MCP_TOKEN")
    legacy = env_file_value(path, "FEED_MCP_AUTH_TOKEN")
    if canonical and legacy and canonical != legacy:
        raise AuditError("Feed token aliases conflict in supplied env")
    if canonical:
        return TokenResolution(canonical, f"{source}_canonical")
    if legacy:
        return TokenResolution(legacy, f"{source}_legacy")
    return TokenResolution(None, f"{source}_missing")


def resolve_feed_token_from_operator_env(path: Path) -> TokenResolution:
    """Resolve Feed aliases from an operator env, including its safe dynamic form."""

    canonical = resolve_operator_env_value(path, "KRW_FEED_MCP_TOKEN")
    legacy = resolve_operator_env_value(path, "FEED_MCP_AUTH_TOKEN")
    if canonical.value and legacy.value and canonical.value != legacy.value:
        raise AuditError("Feed token aliases conflict in operator env")
    if canonical.value:
        return TokenResolution(canonical.value, f"{canonical.source}_canonical")
    if legacy.value:
        return TokenResolution(legacy.value, f"{legacy.source}_legacy")
    if canonical.source == "operator_dynamic_unresolved":
        return canonical
    if legacy.source == "operator_dynamic_unresolved":
        return legacy
    return TokenResolution(None, "operator_missing")


def load_endpoints(operator_root: Path) -> tuple[dict[str, Endpoint], bytes]:
    contract = read_json(operator_root / "runtime" / "local-mcp-gateway-contract.json")
    if contract.get("schema_version") != "krw-agent-local-mcp-gateway-contract/v1":
        raise AuditError("local MCP gateway contract version is unsupported")
    raw_endpoints = contract.get("endpoints")
    if not isinstance(raw_endpoints, list):
        raise AuditError("local MCP gateway contract has no endpoint list")
    loaded: dict[str, Endpoint] = {}
    for raw in raw_endpoints:
        if not isinstance(raw, dict):
            raise AuditError("local MCP gateway endpoint is not an object")
        name = raw.get("name")
        if name not in EXPECTED_ENDPOINTS or name in loaded:
            raise AuditError("local MCP gateway endpoint set is not closed")
        reuse = raw.get("tool_session_reuse")
        if reuse != EXPECTED_REUSE[name]:
            raise AuditError(f"{name} session reuse policy drifted")
        port = raw.get("listen_port")
        if not isinstance(port, int) or not 1 <= port <= 65535:
            raise AuditError(f"{name} has no valid listen port")
        # These values are cross-checked against /readyz below.  They are
        # represented in the gateway contract so a stale endpoint cannot be
        # mistaken for a successful data read.
        loaded[name] = Endpoint(
            name=name,
            port=port,
            reuse=reuse,
            service="",
            protocol_version="",
            build_id="",
            schema_hash="",
            release_hash="",
        )
    if set(loaded) != set(EXPECTED_ENDPOINTS):
        raise AuditError("local MCP gateway contract is missing an endpoint")
    try:
        ca = (operator_root / "tls" / "ca.pem").read_bytes()
    except OSError as exc:
        raise AuditError("local MCP gateway CA certificate is unavailable") from exc
    return loaded, ca


def request(
    endpoint: Endpoint,
    ca: bytes,
    body: dict[str, Any] | None = None,
    token: str | None = None,
    session_id: str | None = None,
    timeout: float = 75.0,
) -> tuple[int, dict[str, str], str, float]:
    headers = {
        "Accept": "application/json, text/event-stream",
        "Mcp-Protocol-Version": "2025-06-18",
    }
    data: bytes | None = None
    method = "GET" if body is None else "POST"
    path = "/readyz" if body is None else "/mcp"
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if session_id:
        headers["Mcp-Session-Id"] = session_id
    context = ssl.create_default_context(cadata=ca.decode("utf-8"))
    req = urllib.request.Request(
        f"https://127.0.0.1:{endpoint.port}{path}",
        data=data,
        headers=headers,
        method=method,
    )
    started = time.monotonic()
    try:
        with urllib.request.urlopen(req, context=context, timeout=timeout) as response:
            return (
                response.status,
                {key.lower(): value for key, value in response.headers.items()},
                response.read(8 * 1024 * 1024).decode("utf-8", "replace"),
                time.monotonic() - started,
            )
    except urllib.error.HTTPError as exc:
        return (
            exc.code,
            {key.lower(): value for key, value in exc.headers.items()},
            exc.read(16 * 1024).decode("utf-8", "replace"),
            time.monotonic() - started,
        )
    except (OSError, urllib.error.URLError) as exc:
        raise AuditError(f"{endpoint.name} transport unavailable") from exc


def parse_message(body: str) -> dict[str, Any]:
    for line in body.splitlines():
        if line.startswith("data: "):
            try:
                value = json.loads(line[6:])
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict):
                return value
    try:
        value = json.loads(body)
    except json.JSONDecodeError as exc:
        raise AuditError("MCP response was neither JSON nor SSE JSON") from exc
    if not isinstance(value, dict):
        raise AuditError("MCP response envelope is not an object")
    return value


def structured_result(message: dict[str, Any]) -> tuple[dict[str, Any] | None, bool, str | None]:
    if message.get("error") is not None:
        return None, True, "json_rpc_error"
    result = message.get("result")
    if not isinstance(result, dict):
        return None, True, "missing_result"
    tool_error = bool(result.get("isError"))
    structured = result.get("structuredContent")
    if isinstance(structured, dict):
        return structured, tool_error, None
    # Older/streaming wrappers may provide only one JSON text content block.
    for block in result.get("content", []):
        if not isinstance(block, dict) or block.get("type") != "text":
            continue
        try:
            value = json.loads(block.get("text", ""))
        except (TypeError, json.JSONDecodeError):
            continue
        if isinstance(value, dict):
            return value, tool_error, None
        if isinstance(value, list):
            return {"items": value}, tool_error, None
    return None, tool_error, "structured_content_missing"


def summarize_payload(payload: dict[str, Any] | None) -> dict[str, Any]:
    if payload is None:
        return {}
    summary: dict[str, Any] = {"keys": sorted(payload)[:32]}
    for key in (
        "results",
        "items",
        "evidence",
        "evidence_units",
        "filings",
        "issues",
        "sources",
        "research_packets",
    ):
        value = payload.get(key)
        if isinstance(value, list):
            summary[f"{key}_count"] = len(value)
    pagination = payload.get("pagination")
    if isinstance(pagination, dict):
        summary["pagination"] = {
            key: pagination[key]
            for key in ("count", "total_count", "has_more", "next_offset", "next_cursor")
            if key in pagination and isinstance(pagination[key], (bool, int, str, type(None)))
        }
    for key in ("status", "answerability", "ticker", "resolved_scope", "has_more"):
        value = payload.get(key)
        if isinstance(value, (str, bool, int)):
            summary[key] = value
    return summary


def check_ready(endpoint: Endpoint, ca: bytes, report: dict[str, Any]) -> None:
    status, _, body, elapsed = request(endpoint, ca)
    if status != 200:
        raise AuditError(f"{endpoint.name} readiness returned HTTP {status}")
    document = parse_message(body)
    required = ("ok", "protocol_version", "build_id", "tool_schema_sha256", "release_manifest_sha256")
    if document.get("ok") is not True or any(not isinstance(document.get(key), str) for key in required[1:]):
        raise AuditError(f"{endpoint.name} readiness document is incomplete")
    if endpoint.reuse == "attested-stateless-v1":
        attestation = document.get("tool_session_contract")
        if not isinstance(attestation, dict) or attestation.get("contract_id") != CONTRACT_ID:
            raise AuditError(f"{endpoint.name} readiness has no stateless attestation")
    report["readiness"] = {
        "http": status,
        "latency_ms": round(elapsed * 1000, 1),
        "ok": True,
        "tool_count": document.get("tool_count"),
    }


def initialize_and_list(
    endpoint: Endpoint,
    ca: bytes,
    token: str | None,
    report: dict[str, Any],
) -> tuple[set[str], str | None]:
    init = {
        "jsonrpc": "2.0",
        "id": "direct-mcp-audit-init",
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "krw-agent-direct-mcp-audit", "version": "1"},
        },
    }
    status, headers, body, elapsed = request(endpoint, ca, init, token)
    if status != 200:
        raise AuditError(f"{endpoint.name} initialize returned HTTP {status}")
    message = parse_message(body)
    if message.get("error") is not None:
        raise AuditError(f"{endpoint.name} initialize returned JSON-RPC error")
    result = message.get("result")
    if not isinstance(result, dict) or result.get("protocolVersion") != "2025-06-18":
        raise AuditError(f"{endpoint.name} initialize protocol mismatch")
    if endpoint.reuse == "attested-stateless-v1":
        session = ((result.get("capabilities") or {}).get("experimental") or {}).get("krwAgentToolSession")
        if not isinstance(session, dict) or session.get("contract_id") != CONTRACT_ID:
            raise AuditError(f"{endpoint.name} initialize has no stateless attestation")
    session_id = headers.get("mcp-session-id")
    list_body = {
        "jsonrpc": "2.0",
        "id": "direct-mcp-audit-list",
        "method": "tools/list",
        "params": {},
    }
    status, _, body, list_elapsed = request(endpoint, ca, list_body, token, session_id)
    if status != 200:
        raise AuditError(f"{endpoint.name} tools/list returned HTTP {status}")
    listed = parse_message(body)
    result = listed.get("result")
    tools = result.get("tools") if isinstance(result, dict) else None
    if not isinstance(tools, list):
        raise AuditError(f"{endpoint.name} tools/list has no tools")
    names = {tool.get("name") for tool in tools if isinstance(tool, dict) and isinstance(tool.get("name"), str)}
    advertised_count = report.get("readiness", {}).get("tool_count")
    if isinstance(advertised_count, int) and len(names) > advertised_count:
        raise AuditError(
            f"{endpoint.name} tools/list returned {len(names)} tools but readiness advertises only {advertised_count}"
        )
    report["initialize"] = {
        "http": 200,
        "latency_ms": round(elapsed * 1000, 1),
        "session_header": bool(session_id),
    }
    report["tools"] = {
        "http": 200,
        "count": len(names),
        "advertised_count": advertised_count,
        "visibility_limited": (
            isinstance(advertised_count, int) and len(names) < advertised_count
        ),
        "latency_ms": round(list_elapsed * 1000, 1),
        "names": sorted(names),
    }
    return names, session_id


def call_tool(
    endpoint: Endpoint,
    ca: bytes,
    token: str | None,
    name: str,
    arguments: dict[str, Any],
    session_id: str | None = None,
) -> dict[str, Any]:
    body = {
        "jsonrpc": "2.0",
        "id": f"direct-mcp-audit-{name}",
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    }
    status, _, raw, elapsed = request(endpoint, ca, body, token, session_id)
    if status != 200:
        raise AuditError(f"{endpoint.name}/{name} returned HTTP {status}")
    message = parse_message(raw)
    payload, tool_error, shape_error = structured_result(message)
    result: dict[str, Any] = {
        "http": status,
        "latency_ms": round(elapsed * 1000, 1),
        "tool_error": tool_error,
    }
    if shape_error:
        result["shape_error"] = shape_error
    if payload is not None:
        result["payload"] = payload
        result["summary"] = summarize_payload(payload)
    return result


def check_ticker(payload: dict[str, Any] | None, ticker: str) -> None:
    if not isinstance(payload, dict):
        return
    observed = payload.get("ticker")
    if isinstance(observed, str) and observed.upper() != ticker.upper():
        raise AuditError("MCP response ticker does not match the requested ticker")
    for key in ("items", "results", "evidence_units"):
        values = payload.get(key)
        if not isinstance(values, list):
            continue
        for row in values:
            if not isinstance(row, dict):
                continue
            row_ticker = row.get("ticker") or row.get("entity")
            if isinstance(row_ticker, str) and row_ticker.upper() != ticker.upper():
                raise AuditError(f"MCP {key} contains a cross-ticker row")


def period_scope_variants(period: str) -> set[str]:
    """Mirror the runtime's annual FY/CY compatibility without quarters."""

    value = str(period or "").strip().upper()
    if not value:
        return set()
    if re.fullmatch(r"(?:FY|CY)(?:19|20)\d{2}Q[1-4]", value):
        return {value}
    match = re.fullmatch(r"(FY|CY)((?:19|20)\d{2})", value)
    if match:
        year = match.group(2)
        return {f"FY{year}", f"CY{year}"}
    return {value}


def source_scope_value(row: dict[str, Any], key: str) -> str:
    """Read a source-filing field before falling back to the row projection.

    Metric rows use the top-level ``period`` for the observed/comparative
    period.  Their source filing period lives under ``document``/``object``;
    treating the projection period as the filing period creates false audit
    failures for a perfectly valid annual metric series.
    """

    for parent_key in ("document", "object"):
        parent = row.get(parent_key)
        if isinstance(parent, dict):
            value = parent.get(key)
            if isinstance(value, str) and value.strip():
                return value.strip()
    value = row.get(key)
    return value.strip() if isinstance(value, str) else ""


def check_explicit_query_scope(
    payload: dict[str, Any] | None,
    *,
    requested_period: str,
    requested_document_type: str,
) -> None:
    """Ensure an explicit read never returns another filing's source object."""

    if not isinstance(payload, dict):
        return
    rows = payload.get("results")
    if not isinstance(rows, list):
        return
    allowed_periods = period_scope_variants(requested_period)
    for row in rows:
        if not isinstance(row, dict):
            continue
        row_period = source_scope_value(row, "period").upper()
        if row_period and row_period not in allowed_periods:
            raise AuditError(
                "ontology explicit period query returned a row outside the requested filing scope"
            )
        row_document_type = source_scope_value(row, "document_type").upper()
        if row_document_type and row_document_type != requested_document_type.upper():
            raise AuditError(
                "ontology explicit document query returned a row outside the requested filing scope"
            )


def run(args: argparse.Namespace) -> dict[str, Any]:
    operator_root = Path(args.operator_root).expanduser().resolve()
    endpoints, ca = load_endpoints(operator_root)
    # The deploy env is only a convenience for local operators.  Explicit
    # process environment values always win; values are never included in the
    # report.
    deploy_env = operator_root / "runtime" / "krw-agent-deploy.env"
    front_env = Path(args.front_env_file).expanduser().resolve() if args.front_env_file else None

    def token_from_front_or_operator(name: str, env_name: str) -> TokenResolution:
        if os.environ.get(env_name):
            return TokenResolution(os.environ[env_name], "process")
        if front_env is not None:
            if name == "feed":
                resolution = resolve_feed_token_from_env(front_env, "front_override")
            else:
                resolution = TokenResolution(env_file_value(front_env, env_name), "front_override")
            if resolution.value:
                return resolution
        if name == "feed":
            return resolve_feed_token_from_operator_env(deploy_env)
        return resolve_operator_env_value(deploy_env, env_name)

    token_resolutions = {
        name: token_from_front_or_operator(name, env_name)
        for name, env_name in TOKEN_ENV.items()
    }
    report: dict[str, Any] = {
        "schema_version": "krw-agent-direct-mcp-audit/v1",
        "provider_calls": 0,
        "ticker": args.ticker.upper(),
        "endpoints": {},
        "quality_scope": {
            "model_called": False,
            "checks": ["transport", "readiness", "tool_shapes", "ticker_consistency", "pagination", "error_vs_empty"],
            "not_checked": ["synthesis_quality", "investment_insight_depth", "factuality_of_model_prose"],
        },
    }
    for name, endpoint in endpoints.items():
        token_resolution = token_resolutions.get(name, TokenResolution(None, "not_required"))
        token = token_resolution.value
        if name in TOKEN_ENV and not token:
            endpoint_report = {
                "status": "skipped",
                "reason": "auth_token_not_available",
                "auth": {"source": token_resolution.source, "result": "missing"},
            }
            report["endpoints"][name] = endpoint_report
            if args.strict_auth:
                raise AuditError(f"{name} token is not available")
            continue
        endpoint_report: dict[str, Any] = {"status": "checked", "reuse": endpoint.reuse}
        if name in TOKEN_ENV:
            endpoint_report["auth"] = {"source": token_resolution.source, "result": "available"}
        check_ready(endpoint, ca, endpoint_report)
        names, session_id = initialize_and_list(endpoint, ca, token, endpoint_report)
        if name in TOKEN_ENV:
            endpoint_report["auth"] = {"source": token_resolution.source, "result": "ok"}
        report["endpoints"][name] = endpoint_report
        if name == "ontology":
            if "krw_ontology_query" not in names or "krw_ontology_query_context" not in names:
                raise AuditError("ontology read tools are missing")
            company = call_tool(endpoint, ca, token, "krw_ontology_company_context", {"ticker": args.ticker, "limit_topics": 8, "include_internal_ids": True}, session_id)
            query = call_tool(endpoint, ca, token, "krw_ontology_query", {"ticker": args.ticker, "topic": "revenue", "document_types": ["10-K", "10-Q"], "limit": 5, "response_detail": "full"}, session_id)
            plan = {
                "question": f"{args.ticker} recent revenue and risk research",
                "intent": "company_research",
                "clauses": [
                    {"clause_id": "revenue_trend", "retrieval_query": f"{args.ticker} revenue sales period comparison", "required_concepts": ["revenue"], "metrics": ["revenue"], "directness": "direct_preferred"},
                    {"clause_id": "risk_factors", "retrieval_query": f"{args.ticker} risk factors competition supply chain regulation", "required_concepts": ["risk factors"], "directness": "direct_preferred"},
                ],
                "tickers": [args.ticker.upper()],
                "document_types": ["10-K", "10-Q"],
                "periods": [],
                "answer_scope": "direct",
                "uncertainty": "medium",
                "limit_results": 12,
                "limit_tickers": 5,
            }
            context = call_tool(endpoint, ca, token, "krw_ontology_query_context", plan, session_id)
            for label, value in (("company_context", company), ("query", query), ("query_context", context)):
                if value.get("payload") is not None and not value.get("tool_error"):
                    check_ticker(value["payload"], args.ticker)
                endpoint_report[label] = {key: value[key] for key in value if key != "payload"}
                if "payload" in value:
                    endpoint_report[label]["summary"] = value.get("summary", {})
            rows = (query.get("payload") or {}).get("results", [])
            object_id = next((row.get("id") for row in rows if isinstance(row, dict) and isinstance(row.get("id"), str)), None)
            if object_id:
                trace = call_tool(endpoint, ca, token, "krw_ontology_trace", {"object_id": object_id, "ticker": args.ticker}, session_id)
                chain = call_tool(endpoint, ca, token, "krw_ontology_chain", {"object_id": object_id, "ticker": args.ticker, "max_depth": 2, "direction": "both", "include_quote_text": False}, session_id)
                endpoint_report["trace"] = {key: trace[key] for key in trace if key != "payload"}
                endpoint_report["chain"] = {key: chain[key] for key in chain if key != "payload"}
                if isinstance(chain.get("payload"), dict):
                    chain_value = chain["payload"].get("chain")
                    if not isinstance(chain_value, dict) or not any(chain_value.get(key) for key in ("evidence_chain", "semantic_neighbors", "temporal_context", "edge_paths")):
                        raise AuditError("ontology chain returned no relationship payload")
            # Use one observed filing as a model-free scope probe.  This is
            # intentionally derived from the response, so the audit remains
            # useful for tickers with different filing calendars and does not
            # hard-code a guessed year.
            first_row = next(
                (
                    row
                    for row in rows
                    if isinstance(row, dict)
                    and source_scope_value(row, "period")
                    and source_scope_value(row, "document_type")
                ),
                None,
            )
            if first_row is not None:
                requested_period = source_scope_value(first_row, "period")
                requested_document_type = source_scope_value(first_row, "document_type")
                scoped_query = call_tool(
                    endpoint,
                    ca,
                    token,
                    "krw_ontology_query",
                    {
                        "ticker": args.ticker,
                        "topic": "revenue",
                        "document_types": [requested_document_type],
                        "periods": [requested_period],
                        "limit": 5,
                        "response_detail": "full",
                    },
                    session_id,
                )
                if scoped_query.get("payload") is not None and not scoped_query.get("tool_error"):
                    check_explicit_query_scope(
                        scoped_query["payload"],
                        requested_period=requested_period,
                        requested_document_type=requested_document_type,
                    )
                endpoint_report["explicit_scope_query"] = {
                    key: scoped_query[key] for key in scoped_query if key != "payload"
                }
                endpoint_report["explicit_scope_query"]["summary"] = scoped_query.get("summary", {})
        elif name == "feed":
            if "list_feed_items" not in names:
                raise AuditError("feed list tool is missing")
            items = call_tool(endpoint, ca, token, "list_feed_items", {"tickers": [args.ticker.upper()], "limit": 3}, session_id)
            if isinstance(items.get("payload"), dict) and not items.get("tool_error"):
                check_ticker(items["payload"], args.ticker)
            endpoint_report["list_feed_items"] = {key: items[key] for key in items if key != "payload"}
            endpoint_report["list_feed_items"]["summary"] = items.get("summary", {})
        elif name == "filings":
            if "search_catalog_filings" not in names:
                raise AuditError("filings search tool is missing")
            # This MCP intentionally covers the event/Form 4 catalog. Annual
            # and quarterly filing evidence is served by ontology.query.
            filings = call_tool(endpoint, ca, token, "search_catalog_filings", {"ticker": args.ticker.upper(), "form_type": "8-K", "limit": 3}, session_id)
            endpoint_report["search_catalog_filings"] = {key: filings[key] for key in filings if key != "payload"}
            endpoint_report["search_catalog_filings"]["summary"] = filings.get("summary", {})
        else:
            if "krw_guru_query_context" not in names:
                raise AuditError("Guru query-context tool is missing")
            guru = call_tool(endpoint, ca, token, "krw_guru_query_context", {"question": f"{args.ticker} business structure and key risks", "ticker": args.ticker.upper(), "author_keys": ["buffett", "marks"], "response_format": "json"}, session_id)
            endpoint_report["query_context"] = {key: guru[key] for key in guru if key != "payload"}
            endpoint_report["query_context"]["summary"] = guru.get("summary", {})
    return report


def main() -> int:
    args = parse_args()
    try:
        report = run(args)
    except AuditError as exc:
        print(json.dumps({"schema_version": "krw-agent-direct-mcp-audit/v1", "status": "failed", "reason": str(exc)}, ensure_ascii=False))
        return 1
    report["status"] = "passed"
    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True))
    else:
        print(f"direct MCP audit: {report['status']} ticker={report['ticker']} model_called=false")
        for name, value in report["endpoints"].items():
            if value.get("status") == "skipped":
                print(f"  {name}: skipped ({value['reason']})")
                continue
            tools = value.get("tools", {})
            auth = value.get("auth")
            auth_suffix = ""
            if isinstance(auth, dict):
                auth_suffix = f" auth={auth.get('result')}/{auth.get('source')}"
            print(
                f"  {name}: ready tools={tools.get('count', 0)} reuse={value.get('reuse')}{auth_suffix}"
            )
            for key in sorted(value):
                if key in {"status", "reuse", "readiness", "initialize", "tools"} or not isinstance(value[key], dict):
                    continue
                summary = value[key].get("summary")
                if summary is not None:
                    print(f"    {key}: tool_error={value[key].get('tool_error', False)} {json.dumps(summary, ensure_ascii=False, separators=(',', ':'))}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
