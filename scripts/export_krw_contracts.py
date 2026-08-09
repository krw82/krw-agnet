#!/usr/bin/env python3
"""Export the canonical krw-ontology Pydantic contracts deterministically.

This is an authoring/CI tool.  Production binaries consume only the generated
artifacts and never import Python or generate schemas at runtime.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import importlib
import inspect
import json
import platform
import sys
import tempfile
from importlib import metadata
from pathlib import Path
from typing import Any, Callable


FORMAT_VERSION = "krw-agent-contract-export/v1"
VECTOR_FORMAT_VERSION = "krw-agent-contract-conformance/v1"
CONTRACT_MODULE = "krw_ontology.mcp_server.contracts"
PYDANTIC_CONTRACTS: tuple[tuple[str, str, str], ...] = (
    ("search-plan/v2", "SearchPlan", "search-plan-v2.json"),
    ("research-state/v2", "ResearchState", "research-state-v2.json"),
    (
        "query-context-input-correction/v1",
        "QueryContextInputCorrection",
        "query-context-input-correction-v1.json",
    ),
)
SERVER_TOOL_CONTRACTS: tuple[tuple[str, str, str], ...] = (
    (
        "ontology-company-context/v1",
        "krw_ontology_company_context",
        "ontology-company-context-v1.json",
    ),
    (
        "ontology-targeted-query/v1",
        "krw_ontology_query",
        "ontology-targeted-query-v1.json",
    ),
    (
        "ontology-trace-input/v1",
        "krw_ontology_trace",
        "ontology-trace-input-v1.json",
    ),
)
# These are kernel-owned, local-only capability contracts. They remain in the
# same bundle as ontology contracts because AgentImage pins one closed registry
# at build time, but they are not MCP server schemas and must not be inferred
# from the external ontology checkout.
LOCAL_JSON_CONTRACTS: tuple[tuple[str, str, str, dict[str, Any]], ...] = (
    (
        "skill-load/v1",
        "krw_skill_load",
        "skill-load-v1.json",
        {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "additionalProperties": False,
            "properties": {
                "skill_id": {
                    "description": "The immutable skill catalog ID. Must match a catalog entry exactly.",
                    "minLength": 1,
                    "pattern": "^[a-z][a-z0-9_]*$",
                    "title": "Skill Id",
                    "type": "string",
                }
            },
            "required": ["skill_id"],
            "title": "SkillLoadInput",
            "type": "object",
        },
    ),
    (
        "skill-content/v1",
        "krw_skill_load",
        "skill-content-v1.json",
        {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "additionalProperties": False,
            "properties": {
                "content": {
                    "description": "The full Markdown body of the requested skill (frontmatter stripped).",
                    "title": "Content",
                    "type": "string",
                },
                "skill_id": {
                    "description": "The requested skill name, echoed back for correlation.",
                    "title": "Skill Id",
                    "type": "string",
                },
            },
            "required": ["skill_id", "content"],
            "title": "SkillContent",
            "type": "object",
        },
    ),
)
ALL_CONTRACTS = PYDANTIC_CONTRACTS + SERVER_TOOL_CONTRACTS
GENERATED_FILES = (
    "schema-bundle.json",
    "conformance-vectors.json",
    "manifest.json",
    "manifest.sha256",
    "bindings.rs",
    "schemas/search-plan-v2.json",
    "schemas/research-state-v2.json",
    "schemas/query-context-input-correction-v1.json",
    "schemas/ontology-company-context-v1.json",
    "schemas/ontology-targeted-query-v1.json",
    "schemas/ontology-trace-input-v1.json",
    "schemas/skill-load-v1.json",
    "schemas/skill-content-v1.json",
)


class ExportFailure(RuntimeError):
    """The canonical source could not be exported safely."""


def _sha256(data: bytes) -> str:
    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def _canonical_bytes(value: Any) -> bytes:
    """Serialize the integer-only RFC 8785 subset emitted by these contracts.

    Pydantic's schemas and the checked conformance inputs contain no floating
    point values.  Refusing floats avoids Python/ECMAScript number formatting
    differences.  The Rust artifact test additionally proves byte-for-byte JCS
    equivalence with ``serde_jcs``.
    """

    def reject_floats(item: Any, path: str = "") -> None:
        if isinstance(item, float):
            raise ExportFailure(f"floating point value is not canonical-safe at {path or '/'}")
        if isinstance(item, dict):
            for key, child in item.items():
                if not isinstance(key, str):
                    raise ExportFailure(f"non-string JSON object key at {path or '/'}")
                reject_floats(child, f"{path}/{key}")
        elif isinstance(item, list):
            for index, child in enumerate(item):
                reject_floats(child, f"{path}/{index}")

    reject_floats(value)
    return json.dumps(
        value,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")


def _file_sha256(path: Path) -> str:
    return _sha256(path.read_bytes())


def _load_authority(source_root: Path) -> tuple[Any, Any, Any, Any]:
    source_root = source_root.resolve()
    package_root = source_root / "src"
    expected = package_root / "krw_ontology/mcp_server/contracts.py"
    if not expected.is_file():
        raise ExportFailure(f"canonical contracts.py not found below {source_root}")

    sys.path.insert(0, str(package_root))
    try:
        contracts = importlib.import_module(CONTRACT_MODULE)
        metric_dictionary = importlib.import_module(
            "krw_ontology.agent_index.metric_dictionary"
        )
        server = importlib.import_module("krw_ontology.mcp_server.server")
        pydantic = importlib.import_module("pydantic")
    finally:
        sys.path.pop(0)

    resolved = Path(inspect.getsourcefile(contracts) or "").resolve()
    if resolved != expected.resolve():
        raise ExportFailure(
            f"imported {CONTRACT_MODULE} from {resolved}, expected {expected.resolve()}"
        )
    expected_server = source_root / "src/krw_ontology/mcp_server/server.py"
    resolved_server = Path(inspect.getsourcefile(server) or "").resolve()
    if resolved_server != expected_server.resolve():
        raise ExportFailure(
            "imported krw_ontology.mcp_server.server from "
            f"{resolved_server}, expected {expected_server.resolve()}"
        )
    return contracts, metric_dictionary, server, pydantic


def _semantic_authority(
    source_root: Path,
    contracts: Any,
    metric_dictionary: Any,
    server: Any,
    pydantic: Any,
) -> dict[str, Any]:
    source_root = source_root.resolve()
    semantic_sources = {
        "krw_ontology/agent_index/metric_dictionary.py": _file_sha256(
            source_root / "src/krw_ontology/agent_index/metric_dictionary.py"
        ),
        "krw_ontology/mcp_server/contracts.py": _file_sha256(
            source_root / "src/krw_ontology/mcp_server/contracts.py"
        ),
        "krw_ontology/mcp_server/server.py": _file_sha256(
            source_root / "src/krw_ontology/mcp_server/server.py"
        ),
    }
    return {
        "module": CONTRACT_MODULE,
        "mcp_contract_version": contracts.MCP_CONTRACT_VERSION,
        "research_state_version": contracts.RESEARCH_STATE_VERSION,
        "pydantic_version": pydantic.__version__,
        "mcp_sdk_version": metadata.version("mcp"),
        "python_implementation": platform.python_implementation(),
        "metric_dictionary_binding": metric_dictionary.metric_dictionary_binding(),
        "semantic_sources": semantic_sources,
    }


def _server_tools(server: Any) -> dict[str, Any]:
    tools = {tool.name: tool for tool in server.mcp._tool_manager.list_tools()}
    missing = [name for _contract_id, name, _file in SERVER_TOOL_CONTRACTS if name not in tools]
    if missing:
        raise ExportFailure(f"canonical MCP server is missing tools: {', '.join(missing)}")
    return tools


def _schema_bundle(
    contracts: Any,
    server: Any,
    authority_hash: str,
) -> tuple[dict[str, Any], dict[str, Any]]:
    schemas: dict[str, Any] = {}
    per_contract: dict[str, Any] = {}
    for contract_id, class_name, file_name in PYDANTIC_CONTRACTS:
        model = getattr(contracts, class_name)
        schema = model.model_json_schema(mode="validation")
        schemas[contract_id] = schema
        per_contract[contract_id] = {
            "authority_kind": "pydantic_model",
            "authority_ref": f"{CONTRACT_MODULE}.{class_name}",
            "schema_path": f"schemas/{file_name}",
            "semantic_validation": "canonical_pydantic_model",
        }
    tools = _server_tools(server)
    for contract_id, tool_name, file_name in SERVER_TOOL_CONTRACTS:
        schemas[contract_id] = copy.deepcopy(tools[tool_name].parameters)
        per_contract[contract_id] = {
            "authority_kind": "mcp_tool_input_schema",
            "authority_ref": tool_name,
            "schema_path": f"schemas/{file_name}",
            "semantic_validation": "canonical_fastmcp_argument_model",
        }
    for contract_id, authority_ref, file_name, schema in LOCAL_JSON_CONTRACTS:
        schemas[contract_id] = copy.deepcopy(schema)
        per_contract[contract_id] = {
            "authority_kind": "json_schema",
            "authority_ref": authority_ref,
            "schema_path": f"schemas/{file_name}",
            "semantic_validation": "canonical_json_schema",
        }
    bundle = {
        "authority_sha256": authority_hash,
        "contracts": schemas,
        "format": FORMAT_VERSION,
        "json_schema_dialect": "https://json-schema.org/draft/2020-12/schema",
        "notice": (
            "JSON Schema covers structural constraints. Canonical Pydantic validators and "
            "the conformance vectors remain authoritative for semantic validation."
        ),
    }
    return bundle, per_contract


def _validation_outcome(model: Any, value: Any) -> dict[str, Any]:
    try:
        normalized = model.model_validate(value).model_dump(mode="json")
    except Exception as exc:  # Pydantic's public ValidationError API is checked below.
        errors_method = getattr(exc, "errors", None)
        if not callable(errors_method):
            raise
        errors = errors_method(include_url=False, include_context=False, include_input=False)
        return {
            "accepted": False,
            "errors": sorted(
                (
                    {
                        "location": [str(part) for part in error.get("loc", ())],
                        "type": str(error.get("type", "unknown")),
                    }
                    for error in errors
                ),
                key=lambda error: (error["location"], error["type"]),
            ),
        }
    return {"accepted": True, "normalized": normalized}


def _function_outcome(function: Callable[[Any], Any], value: Any) -> dict[str, Any]:
    result = function(value)
    dump = getattr(result, "model_dump", None)
    if not callable(dump):
        raise ExportFailure(f"canonical function returned unsupported type {type(result)!r}")
    return {
        "accepted": True,
        "result_model": type(result).__name__,
        "normalized": dump(mode="json"),
    }


def _conformance_vectors(
    contracts: Any,
    server: Any,
    fixture_path: Path,
    authority_hash: str,
) -> dict[str, Any]:
    valid_plan = {
        "question": "How durable is ACME cash generation?",
        "intent": "company_research",
        "clauses": [
            {
                "clause_id": "cash_generation",
                "retrieval_query": "ACME cash generation",
                "required_concepts": ["cash generation"],
                "required": True,
                "tickers": ["acme"],
                "directness": "direct_required",
            }
        ],
        "tickers": [" acme ", "ACME"],
        "document_types": ["10-k", "10-K"],
        "comparison_axes": ["directness", "directness"],
        "limit_results": 8,
    }
    duplicate_clause_plan = copy.deepcopy(valid_plan)
    duplicate = copy.deepcopy(duplicate_clause_plan["clauses"][0])
    duplicate["clause_id"] = "CASH_GENERATION"
    duplicate_clause_plan["clauses"].append(duplicate)

    mixed_clause_plan = {
        "question": "Did ACME revenue and competitive moat improve?",
        "intent": "company_research",
        "clauses": [
            {
                "clause_id": "mixed",
                "retrieval_query": "ACME revenue competitive moat drives revenue",
                "metrics": ["revenue"],
                "required_concepts": ["competitive moat"],
                "required_predicates": ["drives"],
                "required": True,
                "tickers": ["ACME"],
            }
        ],
        "tickers": ["ACME"],
    }

    research_state = json.loads(fixture_path.read_text(encoding="utf-8"))
    research_state_with_extra = copy.deepcopy(research_state)
    research_state_with_extra["unexpected_runtime_field"] = True
    tools = _server_tools(server)
    company_context_argument_model = tools["krw_ontology_company_context"].fn_metadata.arg_model
    query_argument_model = tools["krw_ontology_query"].fn_metadata.arg_model
    trace_argument_model = tools["krw_ontology_trace"].fn_metadata.arg_model

    vector_specs = (
        (
            "search-plan-normalization",
            "search-plan/v2",
            "model_validate",
            valid_plan,
            lambda value: _validation_outcome(contracts.SearchPlan, value),
        ),
        (
            "search-plan-rejects-casefold-duplicate-clause-id",
            "search-plan/v2",
            "model_validate",
            duplicate_clause_plan,
            lambda value: _validation_outcome(contracts.SearchPlan, value),
        ),
        (
            "query-context-returns-typed-mixed-clause-correction",
            "query-context-input-correction/v1",
            "validate_query_context_search_plan",
            mixed_clause_plan,
            lambda value: _function_outcome(
                contracts.validate_query_context_search_plan, value
            ),
        ),
        (
            "research-state-answerable-fixture",
            "research-state/v2",
            "model_validate",
            research_state,
            lambda value: _validation_outcome(contracts.ResearchState, value),
        ),
        (
            "research-state-rejects-extra-field",
            "research-state/v2",
            "model_validate",
            research_state_with_extra,
            lambda value: _validation_outcome(contracts.ResearchState, value),
        ),
        (
            "company-context-requires-a-ticker-and-keeps-server-defaults",
            "ontology-company-context/v1",
            "fastmcp_argument_model_validate",
            {"ticker": "ACME", "limit_topics": 6, "include_internal_ids": False},
            lambda value: _validation_outcome(company_context_argument_model, value),
        ),
        (
            "targeted-query-applies-server-owned-defaults",
            "ontology-targeted-query/v1",
            "fastmcp_argument_model_validate",
            {"topic": "cash generation", "ticker": "ACME", "limit": 4},
            lambda value: _validation_outcome(query_argument_model, value),
        ),
        (
            "trace-requires-object-id",
            "ontology-trace-input/v1",
            "fastmcp_argument_model_validate",
            {"ticker": "ACME"},
            lambda value: _validation_outcome(trace_argument_model, value),
        ),
    )
    vectors = []
    for vector_id, contract_id, operation, input_value, evaluate in vector_specs:
        vectors.append(
            {
                "contract_id": contract_id,
                "expected": evaluate(input_value),
                "id": vector_id,
                "input": input_value,
                "operation": operation,
            }
        )
    return {
        "authority_sha256": authority_hash,
        "format": VECTOR_FORMAT_VERSION,
        "vectors": vectors,
    }


def _rust_bindings(manifest_hash: str, schema_hashes: dict[str, str]) -> bytes:
    by_id = dict(schema_hashes)
    text = f'''// @generated by scripts/export_krw_contracts.py; DO NOT EDIT.
pub const GENERATED_MANIFEST_SHA256: &str = "{manifest_hash}";
pub const SEARCH_PLAN_V2_SCHEMA_SHA256: &str = "{by_id["search-plan/v2"]}";
pub const RESEARCH_STATE_V2_SCHEMA_SHA256: &str = "{by_id["research-state/v2"]}";
pub const QUERY_CONTEXT_INPUT_CORRECTION_V1_SCHEMA_SHA256: &str = "{by_id["query-context-input-correction/v1"]}";
pub const ONTOLOGY_COMPANY_CONTEXT_V1_SCHEMA_SHA256: &str = "{by_id["ontology-company-context/v1"]}";
pub const ONTOLOGY_TARGETED_QUERY_V1_SCHEMA_SHA256: &str = "{by_id["ontology-targeted-query/v1"]}";
pub const ONTOLOGY_TRACE_INPUT_V1_SCHEMA_SHA256: &str = "{by_id["ontology-trace-input/v1"]}";
'''
    return text.encode("utf-8")


def _build_artifacts(source_root: Path, fixture_path: Path) -> dict[str, bytes]:
    contracts, metric_dictionary, server, pydantic = _load_authority(source_root)
    authority = _semantic_authority(
        source_root, contracts, metric_dictionary, server, pydantic
    )
    authority_hash = _sha256(_canonical_bytes(authority))
    schema_bundle, descriptors = _schema_bundle(contracts, server, authority_hash)
    vectors = _conformance_vectors(contracts, server, fixture_path, authority_hash)

    artifacts: dict[str, bytes] = {
        "schema-bundle.json": _canonical_bytes(schema_bundle),
        "conformance-vectors.json": _canonical_bytes(vectors),
    }
    schema_hashes: dict[str, str] = {}
    for contract_id, _authority_name, file_name in ALL_CONTRACTS:
        path = f"schemas/{file_name}"
        raw = _canonical_bytes(schema_bundle["contracts"][contract_id])
        artifacts[path] = raw
        schema_hashes[contract_id] = _sha256(raw)
        descriptors[contract_id]["schema_sha256"] = schema_hashes[contract_id]
    for contract_id, _authority_ref, file_name, _schema in LOCAL_JSON_CONTRACTS:
        path = f"schemas/{file_name}"
        raw = _canonical_bytes(schema_bundle["contracts"][contract_id])
        artifacts[path] = raw
        schema_hashes[contract_id] = _sha256(raw)
        descriptors[contract_id]["schema_sha256"] = schema_hashes[contract_id]

    manifest = {
        "authority": authority,
        "authority_sha256": authority_hash,
        "conformance_vectors": {
            "path": "conformance-vectors.json",
            "sha256": _sha256(artifacts["conformance-vectors.json"]),
        },
        "contracts": descriptors,
        "format": FORMAT_VERSION,
        "schema_bundle": {
            "path": "schema-bundle.json",
            "sha256": _sha256(artifacts["schema-bundle.json"]),
        },
    }
    manifest_bytes = _canonical_bytes(manifest)
    manifest_hash = _sha256(manifest_bytes)
    artifacts["manifest.json"] = manifest_bytes
    artifacts["manifest.sha256"] = f"{manifest_hash}  manifest.json\n".encode()
    artifacts["bindings.rs"] = _rust_bindings(manifest_hash, schema_hashes)
    return artifacts


def _write_artifacts(output_root: Path, artifacts: dict[str, bytes]) -> None:
    output_root.mkdir(parents=True, exist_ok=True)
    for relative, raw in sorted(artifacts.items()):
        path = output_root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(raw)


def _check_artifacts(output_root: Path, artifacts: dict[str, bytes]) -> None:
    failures: list[str] = []
    for relative in GENERATED_FILES:
        expected = artifacts.get(relative)
        if expected is None:
            failures.append(f"generator did not produce declared file: {relative}")
            continue
        path = output_root / relative
        if not path.is_file():
            failures.append(f"missing generated file: {relative}")
        elif path.read_bytes() != expected:
            failures.append(f"stale generated file: {relative}")
    if failures:
        raise ExportFailure("\n".join(failures))


def _default_source_root(script: Path) -> Path:
    return script.resolve().parents[2] / "krw-ontology"


def main() -> int:
    script = Path(__file__)
    repository = script.resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--source-root",
        type=Path,
        default=_default_source_root(script),
        help="read-only checkout containing canonical krw-ontology Pydantic models",
    )
    parser.add_argument(
        "--output-root",
        type=Path,
        default=repository / "contracts/krw-ontology/v2",
    )
    parser.add_argument(
        "--fixture",
        type=Path,
        default=repository
        / "fixtures/vertical-slice/v1/mcp/research-state-answerable.json",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail if checked-in artifacts differ; do not change the repository",
    )
    args = parser.parse_args()

    artifacts = _build_artifacts(args.source_root, args.fixture)
    if args.check:
        # Exercise writing into an isolated tree too, so check mode covers the
        # exact writer without mutating the repository.
        with tempfile.TemporaryDirectory(prefix="krw-contract-export-") as temp:
            isolated = Path(temp) / "v2"
            _write_artifacts(isolated, artifacts)
            for relative, raw in artifacts.items():
                if (isolated / relative).read_bytes() != raw:
                    raise ExportFailure(f"isolated writer mismatch: {relative}")
        _check_artifacts(args.output_root, artifacts)
        print(f"canonical contracts are current: {_sha256(artifacts['manifest.json'])}")
    else:
        _write_artifacts(args.output_root, artifacts)
        print(f"exported canonical contracts: {_sha256(artifacts['manifest.json'])}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ExportFailure as exc:
        print(f"contract export failed: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
