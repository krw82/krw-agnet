"""Typed, descriptor-driven MCP capability registry.

The descriptor is the only place where a logical capability is bound to a
physical MCP name and its read handler. Dispatch itself never branches on a
tool name. This keeps provider, canonical-action, and transport behavior
separate while retaining source-compatible read semantics.
"""

from __future__ import annotations

import asyncio
import hashlib
import inspect
import json
import os
from collections.abc import Awaitable, Callable, Mapping
from dataclasses import dataclass
from enum import Enum
from typing import Any, get_type_hints

from mcp.types import CallToolResult, TextContent, Tool, ToolAnnotations
from pydantic import BaseModel, ConfigDict, ValidationError, create_model

from krw_capability_runtime.guru import lens_selector
from krw_capability_runtime.guru import mcp_tools as guru_tools
from krw_capability_runtime.market import MarketSnapshotRequest, market_snapshot_tool
from krw_capability_runtime.mcp_server import tools as ontology_tools
from krw_capability_runtime.mcp_server.contracts import (
    QueryContextInputCorrection,
    QueryContextInputViolation,
    ResearchState,
    SearchPlan,
    validate_query_context_search_plan,
)

JsonObject = dict[str, Any]
Handler = Callable[..., Any]
DecodedInput = BaseModel | SearchPlan
# Type aliases are evaluated at module import time even with postponed
# annotations, so keep the forward result boundary explicit here.
InputDecoder = Callable[[Mapping[str, Any]], Any]

_MAX_RESULT_BYTES = 4 * 1024 * 1024
_MAX_INPUT_BYTES = 512 * 1024
_READ_ONLY = ToolAnnotations(
    readOnlyHint=True,
    destructiveHint=False,
    idempotentHint=True,
    openWorldHint=False,
)
_HIDDEN_PARAMETERS = frozenset(
    {
        # Deployment binding owns physical roots. A caller must never choose a
        # local path through an MCP argument.
        "root",
        # Canonical agents always consume compact structured JSON. Formatting
        # and private excerpt policy are presentation/deployment concerns.
        "response_format",
        "response_detail",
        "include_private_excerpt",
        "allow_expensive",
    }
)


class CapabilityLane(str, Enum):
    """Bounded execution lanes shared by every MCP session in this process."""

    FAST = "fast"
    BROAD = "broad"
    DIAGNOSTIC = "diagnostic"


@dataclass(frozen=True)
class DispatchOutcome:
    """Canonical result before the MCP codec produces a wire result."""

    text: str
    structured_content: JsonObject | None
    is_error: bool = False

    def as_mcp_result(self) -> CallToolResult:
        return CallToolResult(
            content=[TextContent(type="text", text=self.text)],
            structuredContent=self.structured_content,
            isError=self.is_error,
        )


class RuntimeLanes:
    """Process-wide bounded blocking work admission.

    The imported ontology implementation is synchronous and uses its own
    bounded store pool. Acquiring before ``to_thread`` prevents a burst of
    MCP sessions from creating one Python worker thread per request.
    """

    def __init__(self) -> None:
        logical_cpus = max(1, os.cpu_count() or 1)
        fast = max(1, min(8, logical_cpus - 1 if logical_cpus > 1 else 1))
        broad = max(1, min(2, fast))
        self._semaphores = {
            CapabilityLane.FAST: asyncio.Semaphore(fast),
            CapabilityLane.BROAD: asyncio.Semaphore(broad),
            CapabilityLane.DIAGNOSTIC: asyncio.Semaphore(1),
        }

    async def invoke(self, lane: CapabilityLane, handler: Handler, arguments: JsonObject) -> Any:
        async with self._semaphores[lane]:
            return await asyncio.to_thread(handler, **arguments)

    async def invoke_value(self, lane: CapabilityLane, handler: Handler, value: Any) -> Any:
        """Run one already-decoded canonical value without fabricating an ABI envelope.

        Most imported handlers still naturally receive a closed keyword object.
        `query_context` is deliberately different: its public capability input
        *is* the SearchPlan value.  Passing that value positionally here keeps
        the former `{search_plan: ...}` parameter shape out of the
        canonical runtime entirely.
        """

        async with self._semaphores[lane]:
            return await asyncio.to_thread(handler, value)


@dataclass(frozen=True)
class ToolDescriptor:
    """One immutable typed capability registration."""

    logical_capability_id: str
    mcp_tool_name: str
    title: str
    description: str
    input_model: type[BaseModel]
    lane: CapabilityLane
    handler: Callable[[DecodedInput], Awaitable[DispatchOutcome]]
    input_decoder: InputDecoder | None = None
    output_schema: JsonObject | None = None

    def input_schema(self) -> JsonObject:
        schema = self.input_model.model_json_schema()
        schema["$schema"] = "https://json-schema.org/draft/2020-12/schema"
        schema["additionalProperties"] = False
        return schema

    def as_mcp_tool(self) -> Tool:
        return Tool(
            name=self.mcp_tool_name,
            title=self.title,
            description=self.description,
            inputSchema=self.input_schema(),
            outputSchema=self.output_schema,
            annotations=_READ_ONLY,
        )

    def decode(self, arguments: Mapping[str, Any]) -> DecodedInput | DispatchOutcome:
        if self.input_decoder is not None:
            return self.input_decoder(arguments)
        try:
            return self.input_model.model_validate(arguments)
        except ValidationError as error:
            return _validation_outcome(error)


class CapabilityRegistry:
    """Startup-validated, immutable registry of all online read capabilities."""

    def __init__(self, descriptors: list[ToolDescriptor]) -> None:
        by_name: dict[str, ToolDescriptor] = {}
        by_logical_id: dict[str, ToolDescriptor] = {}
        for descriptor in descriptors:
            if descriptor.mcp_tool_name in by_name:
                raise ValueError(f"duplicate MCP tool name: {descriptor.mcp_tool_name}")
            if descriptor.logical_capability_id in by_logical_id:
                raise ValueError(
                    f"duplicate logical capability: {descriptor.logical_capability_id}"
                )
            by_name[descriptor.mcp_tool_name] = descriptor
            by_logical_id[descriptor.logical_capability_id] = descriptor
        if len(by_name) != 28:
            raise ValueError(f"expected 28 read capabilities, found {len(by_name)}")
        self._by_name = by_name
        self._by_logical_id = by_logical_id

    def list_tools(self) -> list[Tool]:
        return [self._by_name[name].as_mcp_tool() for name in sorted(self._by_name)]

    def descriptors(self) -> tuple[ToolDescriptor, ...]:
        """Return the registry's declared ABI in deterministic wire-name order.

        Readiness identity is derived from the same immutable descriptors that
        serve ``tools/list``.  Exposing this closed ordered view avoids a
        second hand-maintained inventory or a transport-specific tool-name
        branch when constructing the schema-bundle fingerprint.
        """

        return tuple(self._by_name[name] for name in sorted(self._by_name))

    def descriptor(self, mcp_tool_name: str) -> ToolDescriptor:
        return self._by_name[mcp_tool_name]

    async def dispatch(self, name: str, arguments: Mapping[str, Any] | None) -> CallToolResult:
        descriptor = self._by_name.get(name)
        if descriptor is None:
            return _error_outcome(
                code="unknown_capability",
                message="The requested capability is not registered in this release.",
            ).as_mcp_result()
        if not isinstance(arguments, Mapping):
            return _error_outcome(
                code="invalid_tool_input",
                message="Capability arguments must be a JSON object.",
            ).as_mcp_result()
        try:
            input_bytes = len(_canonical_json(arguments).encode("utf-8"))
        except (TypeError, ValueError):
            return _error_outcome(
                code="invalid_tool_input",
                message="Capability arguments must contain JSON-compatible values.",
            ).as_mcp_result()
        if input_bytes > _MAX_INPUT_BYTES:
            return _error_outcome(
                code="tool_input_too_large",
                message="Capability arguments exceed the declared input byte limit.",
            ).as_mcp_result()
        decoded = descriptor.decode(arguments)
        if isinstance(decoded, DispatchOutcome):
            return decoded.as_mcp_result()
        try:
            outcome = await descriptor.handler(decoded)
        except Exception as error:  # noqa: BLE001 - redacted boundary by design
            return _execution_error_outcome(error).as_mcp_result()
        if len(outcome.text.encode("utf-8")) > _MAX_RESULT_BYTES:
            return _error_outcome(
                code="tool_result_too_large",
                message="Capability result exceeds the declared output byte limit.",
            ).as_mcp_result()
        return outcome.as_mcp_result()


def _canonical_json(value: Any) -> str:
    """Stable JSON text for MCP content; Rust verifies RFC 8785 hashes at ABI admission."""

    return json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
    )


def _error_outcome(
    *, code: str, message: str, violations: list[JsonObject] | None = None
) -> DispatchOutcome:
    payload: JsonObject = {
        "status": "input_correction_required",
        "code": code,
        "message": message,
    }
    if violations:
        payload["violations"] = violations
    return DispatchOutcome(text=_canonical_json(payload), structured_content=payload, is_error=True)


def _validation_outcome(error: ValidationError) -> DispatchOutcome:
    violations = [
        {
            "field": ".".join(str(part) for part in item["loc"]),
            "rule": str(item["type"]),
            "message": str(item["msg"]),
        }
        for item in error.errors(include_url=False)
    ]
    return _error_outcome(
        code="invalid_tool_input",
        message="Capability input does not satisfy its declared contract.",
        violations=violations,
    )


def _execution_error_outcome(error: Exception) -> DispatchOutcome:
    error_fingerprint = hashlib.sha256(
        f"{type(error).__name__}:{error}".encode("utf-8", "replace")
    ).hexdigest()[:16]
    return _error_outcome(
        code="capability_execution_failed",
        message="The read capability did not complete; retry only if the run policy permits it.",
        violations=[{"error_fingerprint": error_fingerprint}],
    )


def _model_from_handler(
    model_name: str,
    handler: Handler,
    *,
    exposed_parameters: frozenset[str] = frozenset(),
) -> type[BaseModel]:
    """Compile a strict Pydantic input model from one typed read-handler signature."""

    signature = inspect.signature(handler)
    hints = get_type_hints(handler)
    fields: dict[str, tuple[Any, Any]] = {}
    for parameter in signature.parameters.values():
        if parameter.kind is parameter.VAR_POSITIONAL:
            raise TypeError(f"unbounded handler parameter in {handler.__name__}: {parameter.name}")
        if parameter.kind is parameter.VAR_KEYWORD:
            # Some imported implementations keep a private ``**extra_args``
            # extension point for their own callers. It is deliberately not
            # representable on MCP: the descriptor schema remains closed.
            continue
        if parameter.name in _HIDDEN_PARAMETERS and parameter.name not in exposed_parameters:
            continue
        annotation = hints.get(parameter.name, Any)
        default = ... if parameter.default is inspect.Parameter.empty else parameter.default
        fields[parameter.name] = (annotation, default)
    return create_model(
        model_name,
        __config__=ConfigDict(extra="forbid", validate_default=True),
        **fields,
    )


def _outcome_from_handler_value(value: Any) -> DispatchOutcome:
    if isinstance(value, BaseModel):
        payload = value.model_dump(mode="json", by_alias=True, exclude_none=True)
        if not isinstance(payload, dict):
            payload = {"value": payload}
        return DispatchOutcome(text=_canonical_json(payload), structured_content=payload)
    if isinstance(value, Mapping):
        payload = dict(value)
        return DispatchOutcome(text=_canonical_json(payload), structured_content=payload)
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError:
            return DispatchOutcome(text=value, structured_content={"text": value})
        payload = parsed if isinstance(parsed, dict) else {"value": parsed}
        return DispatchOutcome(text=_canonical_json(parsed), structured_content=payload)
    raise TypeError(f"unsupported read-handler result type: {type(value).__name__}")


def _standard_descriptor(
    *,
    logical_capability_id: str,
    mcp_tool_name: str,
    title: str,
    description: str,
    source_handler: Handler,
    lane: CapabilityLane,
    runtime_lanes: RuntimeLanes,
    exposed_parameters: frozenset[str] = frozenset(),
) -> ToolDescriptor:
    input_model = _model_from_handler(
        f"{source_handler.__name__.title().replace('_', '')}Input",
        source_handler,
        exposed_parameters=exposed_parameters,
    )

    async def handler(decoded: DecodedInput) -> DispatchOutcome:
        assert isinstance(decoded, BaseModel)
        arguments = decoded.model_dump(mode="python", exclude_none=False)
        value = await runtime_lanes.invoke(lane, source_handler, arguments)
        return _outcome_from_handler_value(value)

    return ToolDescriptor(
        logical_capability_id=logical_capability_id,
        mcp_tool_name=mcp_tool_name,
        title=title,
        description=description,
        input_model=input_model,
        lane=lane,
        handler=handler,
    )


def _root_search_plan_decoder(arguments: Mapping[str, Any]) -> SearchPlan | DispatchOutcome:
    # Deliberately reject the obsolete parameter envelope. We never unwrap it:
    # the trusted canonical action and physical MCP share the SearchPlan root
    # contract, while the provider-facing ResearchIntent is compiled before
    # this transport boundary.
    if set(arguments) == {"search_plan"}:
        correction = QueryContextInputCorrection(
            message="query_context accepts SearchPlan as its root arguments object, not under search_plan.",
            violations=[
                QueryContextInputViolation(
                    field="search_plan",
                    rule="invalid_search_plan",
                    message="The legacy search_plan envelope is not part of this MCP contract.",
                    required_change="Send question, intent, clauses, and the remaining SearchPlan fields directly at arguments root.",
                )
            ],
        )
        payload = correction.model_dump(mode="json", exclude_none=True)
        return DispatchOutcome(
            text=_canonical_json(payload), structured_content=payload, is_error=True
        )
    parsed = validate_query_context_search_plan(arguments)
    if isinstance(parsed, QueryContextInputCorrection):
        payload = parsed.model_dump(mode="json", exclude_none=True)
        return DispatchOutcome(
            text=_canonical_json(payload), structured_content=payload, is_error=True
        )
    return parsed


def _query_context_payload(state: ResearchState) -> JsonObject:
    """Serialize a compact state while preserving its complete plan echo.

    Evidence records intentionally omit ``None`` values to keep the model and
    MCP wire bounded.  The embedded ``SearchPlan`` is different: it is the
    capability's exact, Pydantic-normalized echo of the dispatched research
    program.  Omitting nullable plan defaults would turn a lossless semantic
    round trip into an ambiguous sparse representation and forces every agent
    implementation to guess transport defaults.  Keep only this small control
    object complete; all evidence remains compact.
    """

    payload = state.model_dump(mode="json", by_alias=True, exclude_none=True)
    payload["plan"] = state.plan.model_dump(mode="json", by_alias=True, exclude_none=False)
    return payload


def _query_context_descriptor(runtime_lanes: RuntimeLanes) -> ToolDescriptor:
    async def handler(decoded: DecodedInput) -> DispatchOutcome:
        assert isinstance(decoded, SearchPlan)
        state = await runtime_lanes.invoke_value(
            CapabilityLane.BROAD,
            ontology_tools.query_context_from_search_plan,
            decoded,
        )
        if not isinstance(state, ResearchState):
            raise TypeError("query_context did not return ResearchState")
        payload = _query_context_payload(state)
        return DispatchOutcome(text=_canonical_json(payload), structured_content=payload)

    return ToolDescriptor(
        logical_capability_id="ontology.query_context",
        mcp_tool_name="krw_ontology_query_context",
        title="Plan KRW ontology answer context",
        description="Execute one explicit SearchPlan and return compact ResearchState v2.",
        input_model=SearchPlan,
        lane=CapabilityLane.BROAD,
        handler=handler,
        input_decoder=_root_search_plan_decoder,
        output_schema={
            "anyOf": [
                ResearchState.model_json_schema(),
                QueryContextInputCorrection.model_json_schema(),
            ]
        },
    )


def _market_snapshot_descriptor(runtime_lanes: RuntimeLanes) -> ToolDescriptor:
    """Register the fixed advisory-market router without exposing a source knob."""

    async def handler(decoded: DecodedInput) -> DispatchOutcome:
        assert isinstance(decoded, MarketSnapshotRequest)
        value = await runtime_lanes.invoke(
            CapabilityLane.BROAD,
            market_snapshot_tool,
            decoded.model_dump(mode="python"),
        )
        return _outcome_from_handler_value(value)

    return ToolDescriptor(
        logical_capability_id="market.snapshot",
        mcp_tool_name="krw_market_snapshot",
        title="Return current market snapshot",
        description=(
            "Return compact timestamped price and valuation context for one ticker; "
            "advisory research data only."
        ),
        input_model=MarketSnapshotRequest,
        lane=CapabilityLane.BROAD,
        handler=handler,
    )


def build_registry() -> CapabilityRegistry:
    """Build the shared registry for 13 ontology, 1 market, and 14 Guru tools."""

    lanes = RuntimeLanes()
    standard = _standard_descriptor
    descriptors = [
        standard(
            logical_capability_id="ontology.catalog",
            mcp_tool_name="krw_ontology_catalog",
            title="List KRW ontology companies and documents",
            description="List indexed companies, documents, and period metadata.",
            source_handler=ontology_tools.catalog_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="ontology.index_context",
            mcp_tool_name="krw_ontology_index_context",
            title="Return KRW ontology index context",
            description="Return schema, capabilities, coverage, and answerability policy.",
            source_handler=ontology_tools.index_context_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="ontology.company_context",
            mcp_tool_name="krw_ontology_company_context",
            title="Return KRW ontology company topic context",
            description="Return evidence-derived company topic context.",
            source_handler=ontology_tools.company_context_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        _market_snapshot_descriptor(lanes),
        _query_context_descriptor(lanes),
        standard(
            logical_capability_id="ontology.query",
            mcp_tool_name="krw_ontology_query",
            title="Search KRW ontology evidence",
            description=(
                "Search accepted ontology evidence with explicit filters. "
                "Compact is the default. Choose response_detail=full in this same "
                "precise, bounded call when the user's main question or a primary "
                "objective materially benefits from exact source text, numeric basis, "
                "period/scope detail, or lineage. The choice does not widen the "
                "authenticated ticker, document, period, or object scope."
            ),
            source_handler=ontology_tools.query_tool,
            lane=CapabilityLane.BROAD,
            runtime_lanes=lanes,
            # Detail is a model-owned research-depth choice. Scope and limits
            # remain kernel-canonicalized at the Rust boundary.
            exposed_parameters=frozenset({"response_detail"}),
        ),
        standard(
            logical_capability_id="ontology.topic_map",
            mcp_tool_name="krw_ontology_topic_map",
            title="Discover KRW ontology search topics",
            description="Return company-specific vocabulary for research planning.",
            source_handler=ontology_tools.topic_map_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="ontology.retrieve",
            mcp_tool_name="krw_ontology_retrieve",
            title="Plan and retrieve KRW ontology evidence",
            description="Run the deterministic local retrieval planner.",
            source_handler=ontology_tools.retrieve_tool,
            lane=CapabilityLane.BROAD,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="ontology.trace",
            mcp_tool_name="krw_ontology_trace",
            title="Trace KRW ontology object evidence",
            description="Trace an object to source evidence and quality metadata.",
            source_handler=ontology_tools.trace_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="ontology.verify_evidence",
            mcp_tool_name="krw_ontology_verify_evidence",
            title="Verify KRW ontology evidence lineage",
            description="Verify exact object identifiers and return a bounded evidence pack.",
            source_handler=ontology_tools.verify_evidence_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="ontology.chain",
            mcp_tool_name="krw_ontology_chain",
            title="Trace KRW ontology object relationship chain",
            description="Return bounded evidence and relationship chains.",
            source_handler=ontology_tools.chain_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="ontology.quality",
            mcp_tool_name="krw_ontology_quality",
            title="Inspect KRW ontology quality signals",
            description="Return rejected-object and quality events.",
            source_handler=ontology_tools.quality_tool,
            lane=CapabilityLane.DIAGNOSTIC,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="ontology.compare",
            mcp_tool_name="krw_ontology_compare",
            title="Compare companies in KRW ontology",
            description="Compare two or more companies by evidence topic or metric.",
            source_handler=ontology_tools.compare_tool,
            lane=CapabilityLane.BROAD,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="ontology.plan_query",
            mcp_tool_name="krw_ontology_plan_query",
            title="Validate KRW ontology SearchPlan",
            description="Validate and normalize a SearchPlan without retrieval.",
            source_handler=ontology_tools.plan_query_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.status",
            mcp_tool_name="krw_guru_status",
            title="Return KRW guru advisor status",
            description="Return reviewed Guru ontology status.",
            source_handler=guru_tools.guru_status_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.index_context",
            mcp_tool_name="krw_guru_index_context",
            title="Return KRW guru index context",
            description="Return Guru shard index status and runtime policy.",
            source_handler=guru_tools.guru_index_context_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.search",
            mcp_tool_name="krw_guru_search",
            title="Search reviewed KRW guru ontology",
            description="Search Guru lenses, consultation, and data needs.",
            source_handler=guru_tools.guru_search_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.query_context",
            mcp_tool_name="krw_guru_query_context",
            title="Build Guru advisor research pack",
            description="Build a compact Guru research pack.",
            source_handler=guru_tools.guru_query_context_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.company_brief",
            mcp_tool_name="krw_guru_company_brief",
            title="Build KRW company filing brief from Guru lenses",
            description="Build a company filing brief from sealed Guru workflow inputs.",
            source_handler=guru_tools.guru_company_brief_tool,
            lane=CapabilityLane.BROAD,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.company_pack",
            mcp_tool_name="krw_guru_company_pack",
            title="Build Guru company research pack",
            description="Build company-aware Guru answer context.",
            source_handler=guru_tools.guru_company_pack_tool,
            lane=CapabilityLane.BROAD,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.review_company_evidence",
            mcp_tool_name="krw_guru_review_company_evidence",
            title="Review company evidence through Guru lenses",
            description="Review sealed company evidence through Guru lenses.",
            source_handler=guru_tools.guru_review_company_evidence_tool,
            lane=CapabilityLane.BROAD,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.select_lenses",
            mcp_tool_name="krw_guru_select_lenses",
            title="Select relevant Guru ontology lenses",
            description="Select lenses and evidence hooks for a question.",
            source_handler=lens_selector.guru_select_lenses_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.context",
            mcp_tool_name="krw_guru_context",
            title="Build Guru advisor consultation context",
            description="Build a compact Guru consultation context.",
            source_handler=guru_tools.guru_context_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.trace",
            mcp_tool_name="krw_guru_trace",
            title="Trace one Guru ontology object",
            description="Trace one selected Guru object to source support.",
            source_handler=guru_tools.guru_trace_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.chain",
            mcp_tool_name="krw_guru_chain",
            title="Return bounded Guru ontology chain",
            description="Return bounded neighbors around one Guru object.",
            source_handler=guru_tools.guru_chain_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.evidence",
            mcp_tool_name="krw_guru_evidence",
            title="Return Guru object source support",
            description="Return one Guru object with source support.",
            source_handler=guru_tools.guru_evidence_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.data_needs",
            mcp_tool_name="krw_guru_data_needs",
            title="Return filing evidence needs for Guru consultation",
            description="Return Guru-derived filing evidence requirements.",
            source_handler=guru_tools.guru_data_needs_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
        standard(
            logical_capability_id="guru.eval_questions",
            mcp_tool_name="krw_guru_eval_questions",
            title="List KRW Guru advisor eval questions",
            description="List reviewed Guru advisor evaluation questions.",
            source_handler=guru_tools.guru_eval_questions_tool,
            lane=CapabilityLane.FAST,
            runtime_lanes=lanes,
        ),
    ]
    return CapabilityRegistry(descriptors)
