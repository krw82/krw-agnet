from __future__ import annotations

import asyncio

import pytest

from krw_capability_runtime.mcp_server.contracts import ResearchState, SearchPlan
from krw_capability_runtime.transport.mcp.descriptors import _query_context_payload, build_registry
from krw_capability_runtime.transport.mcp.http import create_http_app
from krw_capability_runtime.transport.mcp.identity import (
    CapabilityServiceIdentity,
    ServiceIdentityError,
    assert_expected_identity,
    build_service_identity,
    tool_schema_bundle,
)
from krw_capability_runtime.transport.mcp.server import create_server


def _test_identity() -> CapabilityServiceIdentity:
    return CapabilityServiceIdentity(
        build_id="test-capabilityd-build",
        tool_schema_sha256="sha256:" + "a" * 64,
        release_manifest_sha256="sha256:" + "b" * 64,
    )


def test_registry_has_exact_canonical_inventory() -> None:
    registry = build_registry()
    tools = registry.list_tools()

    assert len(tools) == 28
    assert {tool.name for tool in tools} >= {
        "krw_ontology_query_context",
        "krw_ontology_query",
        "krw_ontology_trace",
        "krw_market_snapshot",
        "krw_guru_company_brief",
        "krw_guru_review_company_evidence",
    }


def test_query_context_uses_root_search_plan_schema() -> None:
    registry = build_registry()
    schema = registry.descriptor("krw_ontology_query_context").input_schema()

    assert schema["title"] == "SearchPlan"
    assert "question" in schema["properties"]
    assert "clauses" in schema["properties"]
    assert "search_plan" not in schema["properties"]
    assert schema["additionalProperties"] is False


def test_wrapped_search_plan_is_rejected_without_unwrap() -> None:
    registry = build_registry()
    result = asyncio.run(registry.dispatch("krw_ontology_query_context", {"search_plan": {}}))

    assert result.isError is True
    assert result.structuredContent is not None
    assert result.structuredContent["code"] == "search_plan_validation_failed"
    assert result.structuredContent["violations"][0]["field"] == "search_plan"


def test_query_context_keeps_a_complete_canonical_plan_echo() -> None:
    """Only the plan echo retains null defaults; evidence payloads stay compact."""

    plan = SearchPlan.model_validate(
        {
            "question": "How durable is ACME cash generation?",
            "intent": "company_research",
            "clauses": [
                {
                    "clause_id": "cash_generation",
                    "retrieval_query": "ACME cash generation",
                    "required_concepts": ["cash generation"],
                }
            ],
        }
    )
    state = ResearchState.model_validate(
        {
            "plan": plan,
            "resolved_scope": {},
            "answerability": {
                "status": "not_answerable",
                "strong_claim_allowed": False,
                "required_clause_count": 1,
                "covered_required_clause_count": 0,
                "requires_direct_evidence": True,
            },
            "clause_coverage": [],
            "evidence_units": [],
        }
    )

    payload = _query_context_payload(state)
    echoed_plan = payload["plan"]
    assert echoed_plan["universe"] is None
    assert echoed_plan["clauses"][0]["calculation_window"] is None
    # Non-plan nulls remain omitted from the compact evidence response.
    assert "release_id" not in payload


def test_deployment_owned_or_expensive_knobs_are_not_mcp_inputs() -> None:
    registry = build_registry()
    forbidden = {
        "root",
        "response_format",
        "response_detail",
        "include_private_excerpt",
        "allow_expensive",
    }

    for tool in registry.list_tools():
        assert forbidden.isdisjoint(tool.inputSchema.get("properties", {})), tool.name


def test_low_level_server_can_be_created_from_registry() -> None:
    server = create_server(build_registry(), server_version=_test_identity().build_id)
    assert server.name == "krw-capabilityd"


def test_streamable_http_daemon_exposes_one_shared_registry_health() -> None:
    from starlette.testclient import TestClient

    identity = _test_identity()
    with TestClient(create_http_app(registry=build_registry(), identity=identity)) as client:
        response = client.get("/healthz")

    assert response.status_code == 200
    assert response.json() == identity.readiness_document(tool_count=28)


def test_streamable_http_protocol_and_tools_list_match_the_declared_bundle() -> None:
    """Exercise the actual MCP server boundary, not only its ASGI health route."""

    from starlette.testclient import TestClient

    identity = _test_identity()
    registry = build_registry()
    headers = {
        "Accept": "application/json, text/event-stream",
        "Content-Type": "application/json",
        "MCP-Protocol-Version": "2025-06-18",
        "Origin": "https://capability-test.invalid",
    }
    with TestClient(create_http_app(registry=registry, identity=identity)) as client:
        initialized = client.post(
            "/mcp",
            headers=headers,
            json={
                "jsonrpc": "2.0",
                "id": "initialize-1",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "capability-contract-test", "version": "1"},
                },
            },
        )
        assert initialized.status_code == 200
        initialized_body = initialized.json()["result"]
        assert initialized_body["protocolVersion"] == "2025-06-18"
        assert initialized_body["serverInfo"]["version"] == identity.build_id
        session_id = initialized.headers["mcp-session-id"]
        tools = client.post(
            "/mcp",
            headers={**headers, "mcp-session-id": session_id},
            json={
                "jsonrpc": "2.0",
                "id": "tools-list-1",
                "method": "tools/list",
                "params": {},
            },
        )

    assert tools.status_code == 200
    actual_tools = tools.json()["result"]["tools"]
    declared_tools = [record["tool"] for record in tool_schema_bundle(registry)["tools"]]
    assert actual_tools == declared_tools


def test_schema_bundle_is_derived_from_the_complete_descriptor_registry() -> None:
    first = tool_schema_bundle(build_registry())
    second = tool_schema_bundle(build_registry())

    assert first == second
    assert first["schema_version"] == "krw-capabilityd/tool-schema-bundle/v1"
    assert len(first["tools"]) == 28
    query_context = next(
        record
        for record in first["tools"]
        if record["logical_capability_id"] == "ontology.query_context"
    )
    schema = query_context["tool"]["inputSchema"]
    assert schema["title"] == "SearchPlan"
    assert "search_plan" not in schema["properties"]


def test_identity_requires_verified_release_manifest_pin() -> None:
    registry = build_registry()
    verification = {
        "ok": True,
        "release_manifest_sha256": "sha256:" + "c" * 64,
    }
    identity = build_service_identity(registry, verification)

    assert identity.release_manifest_sha256 == verification["release_manifest_sha256"]
    assert identity.tool_schema_sha256.startswith("sha256:")
    assert identity.tool_schema_sha256 != "sha256:" + "0" * 64

    with pytest.raises(ServiceIdentityError, match="release_manifest_sha256"):
        build_service_identity(registry, {"ok": True})


def test_expected_identity_pins_fail_closed_when_they_drift() -> None:
    identity = _test_identity()
    assert_expected_identity(
        identity,
        require_all=True,
        environ={
            "KRW_CAPABILITYD_EXPECTED_BUILD_ID": identity.build_id,
            "KRW_CAPABILITYD_EXPECTED_TOOL_SCHEMA_SHA256": identity.tool_schema_sha256,
            "KRW_CAPABILITYD_EXPECTED_RELEASE_MANIFEST_SHA256": identity.release_manifest_sha256,
        },
    )
    with pytest.raises(ServiceIdentityError, match="EXPECTED_TOOL_SCHEMA"):
        assert_expected_identity(
            identity,
            require_all=True,
            environ={
                "KRW_CAPABILITYD_EXPECTED_BUILD_ID": identity.build_id,
                "KRW_CAPABILITYD_EXPECTED_TOOL_SCHEMA_SHA256": "sha256:" + "d" * 64,
                "KRW_CAPABILITYD_EXPECTED_RELEASE_MANIFEST_SHA256": identity.release_manifest_sha256,
            },
        )
