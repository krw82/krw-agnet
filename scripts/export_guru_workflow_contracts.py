#!/usr/bin/env python3
"""Export the audited default Guru workflow ABI.

The current TypeScript host and Python Guru MCP remain the semantic
authorities.  The schemas below are a deliberately closed projection of the
default, read-only path:

    query_context -> company_brief -> company filing reads -> evidence review

The exporter pins every authority file so source drift fails before artifacts
can be regenerated.  It intentionally contains no legacy company-pack path
and no ontology ``verify_evidence`` capability.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import tempfile
from pathlib import Path
from typing import Any


FORMAT = "krw-guru-workflow-contract-export/v1"
CONFORMANCE_FORMAT = "krw-guru-workflow-contract-conformance/v1"
DIALECT = "https://json-schema.org/draft/2020-12/schema"

FRONT_SOURCE_PINS = {
    "src/lib/agent/guru-investigation-contracts.ts": "49de4f8b429af68cd050874a65aae854d58c70e99aaf07762d3b99f4edf1ee4d",
    "src/lib/agent/runner.ts": "3d5583617f74f74973858e1d3a297e6123061a348215997fcb48d526142ba786",
    "src/lib/ontology/catalog.ts": "a9aef5e96ae16351becdd251f120b1bdfa0b5a5394cdcc32a331a095d9c4810f",
}

ONTOLOGY_SOURCE_PINS = {
    "src/krw_ontology/guru/company_bridge.py": "f06b1825886494c6c887585a6b172f05d05c9171a640069f0ee400739355df35",
    "src/krw_ontology/guru/company_context.py": "cc9cb009183a7396da0dd2fa766bf15b083a38cbc525cbe95b75f5bcdb3e7d65",
    "src/krw_ontology/guru/mcp_server.py": "2309d617c3cc0baf381b66991e135178b80a268447cb72b22c511b0136b986d2",
    "src/krw_ontology/guru/mcp_tools.py": "bde09b0d32b9e7ff5c340f4bcf5e03a7cd20e98ba2600e8ae817a55c9e05f79c",
    "src/krw_ontology/guru/models.py": "10c330398e2836ccc7b8ffe79c9c3ec5cd7766d4f40c1251199e77818b9b97ba",
}

AUTHOR_KEYS = ["buffett", "marks", "ackman", "flatt", "terry_smith"]
RESEARCH_STATUSES = [
    "sufficient_lens",
    "partial_lens",
    "ontology_gap",
    "needs_clarification",
    "needs_company_evidence",
]
CORRECTION_CODES = [
    "exactly_one_key_question_required",
    "invalid_agent_analysis",
    "invalid_company_research_context",
    "invalid_key_question",
    "missing_agent_analysis",
    "missing_company_research_context",
    "missing_investigation_brief",
    "question_scoped_evidence_required",
    "sealed_question_context_required",
    "sealed_question_coverage_required",
    "contextual_evidence_cannot_support",
    "unresolved_verdict_must_not_cite_evidence",
]


def canonical(value: Any) -> bytes:
    # These artifacts contain no floating schema literals.  Sorted UTF-8 JSON
    # with compact separators is byte-identical to RFC 8785 for this domain.
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def sha(data: bytes) -> str:
    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def bare_sha(value: Any) -> str:
    return hashlib.sha256(canonical(value)).hexdigest()


def string(
    *,
    maximum: int,
    minimum: int = 0,
    pattern: str | None = None,
    enum: list[str] | None = None,
    const: str | None = None,
) -> dict[str, Any]:
    result: dict[str, Any] = {"type": "string", "maxLength": maximum}
    if minimum:
        result["minLength"] = minimum
    if pattern:
        result["pattern"] = pattern
    if enum:
        result["enum"] = enum
    if const:
        result["const"] = const
    return result


def integer(minimum: int, maximum: int) -> dict[str, Any]:
    return {"type": "integer", "minimum": minimum, "maximum": maximum}


def array(
    items: dict[str, Any],
    maximum: int,
    minimum: int = 0,
    *,
    unique: bool = False,
) -> dict[str, Any]:
    result: dict[str, Any] = {
        "type": "array",
        "items": items,
        "maxItems": maximum,
    }
    if minimum:
        result["minItems"] = minimum
    if unique:
        result["uniqueItems"] = True
    return result


def obj(
    properties: dict[str, Any],
    required: list[str] | None = None,
    *,
    maximum: int | None = None,
) -> dict[str, Any]:
    result: dict[str, Any] = {
        "type": "object",
        "properties": properties,
        "required": required if required is not None else list(properties),
        "additionalProperties": False,
    }
    if maximum is not None:
        result["maxProperties"] = maximum
    return result


def open_obj(maximum: int = 128) -> dict[str, Any]:
    return {
        "type": "object",
        "additionalProperties": {"$ref": "#/$defs/json_value"},
        "maxProperties": maximum,
    }


def nullable(schema: dict[str, Any]) -> dict[str, Any]:
    return {"anyOf": [schema, {"type": "null"}]}


def ref(name: str) -> dict[str, str]:
    return {"$ref": f"#/$defs/{name}"}


def root(
    contract_id: str,
    body: dict[str, Any],
    defs: dict[str, Any] | None = None,
) -> dict[str, Any]:
    result = {
        "$id": f"urn:krw-agent:contract:{contract_id}",
        "$schema": DIALECT,
        **body,
    }
    if defs:
        result["$defs"] = defs
    return result


IDENTIFIER = string(
    maximum=256,
    minimum=1,
    pattern=r"^[A-Za-z0-9][A-Za-z0-9_.:-]*$",
)
STABLE_IDENTIFIER = string(
    maximum=256,
    minimum=1,
    pattern=r"^[A-Za-z0-9][A-Za-z0-9_.-]*$",
)
TICKER = string(
    maximum=32,
    minimum=1,
    pattern=r"^[A-Z][A-Z0-9.-]{0,31}$",
)
BARE_HASH = string(maximum=64, minimum=64, pattern=r"^[0-9a-f]{64}$")
QUESTION_ID = string(maximum=18, minimum=18, pattern=r"^q_[0-9a-f]{16}$")
AUTHOR = string(maximum=11, enum=AUTHOR_KEYS)
NONEMPTY = string(maximum=4_000, minimum=1)
SHORT_TEXT = string(maximum=1_000, minimum=1)

JSON_VALUE: dict[str, Any] = {
    "anyOf": [
        {"type": "null"},
        {"type": "boolean"},
        {"type": "number"},
        string(maximum=32_768),
        {"type": "array", "items": ref("json_value"), "maxItems": 256},
        {
            "type": "object",
            "additionalProperties": ref("json_value"),
            "maxProperties": 128,
        },
    ]
}


def common_defs() -> dict[str, Any]:
    anchor = obj(
        {
            "anchor_id": STABLE_IDENTIFIER,
            "kind": string(
                maximum=20,
                enum=[
                    "business_description",
                    "primary_activity",
                    "product",
                    "segment",
                    "revenue_logic",
                    "sector",
                ],
            ),
            "text": string(maximum=4_000, minimum=1),
        }
    )
    filing_availability = obj(
        {
            "has_current_filing": {"type": "boolean"},
            "has_annual_baseline": {"type": "boolean"},
        }
    )
    light_context = obj(
        {
            "format": string(
                maximum=33,
                const="krw-guru-light-company-context/v1",
            ),
            "ticker": TICKER,
            "company_name": string(maximum=512, minimum=1),
            "sector": nullable(string(maximum=256, minimum=1)),
            "industry": nullable(string(maximum=256, minimum=1)),
            "business_description": string(maximum=8_000, minimum=1),
            "primary_activities": array(string(maximum=1_000, minimum=1), 6),
            "products_or_segments": array(string(maximum=1_000, minimum=1), 8),
            "revenue_logic": nullable(string(maximum=4_000, minimum=1)),
            "context_anchors": array(ref("company_context_anchor"), 16, 1),
            "filing_availability": ref("filing_availability"),
        },
        required=[
            "format",
            "ticker",
            "company_name",
            "business_description",
            "primary_activities",
            "products_or_segments",
            "context_anchors",
            "filing_availability",
        ],
    )
    draft = obj(
        {
            "question": NONEMPTY,
            "guru_principle_ids": array(IDENTIFIER, 16, 1, unique=True),
            "company_context_anchor_ids": array(
                STABLE_IDENTIFIER,
                16,
                1,
                unique=True,
            ),
            "hypothesis": NONEMPTY,
            "counter_hypothesis": NONEMPTY,
            "evidence_needed": array(SHORT_TEXT, 16, 1, unique=True),
            "strengthens_if": NONEMPTY,
            "weakens_if": NONEMPTY,
            "why_material": NONEMPTY,
            "decision_role": string(maximum=12, const="main_tension"),
        }
    )
    question = obj({**draft["properties"], "question_id": QUESTION_ID})
    brief = obj(
        {
            "format": string(
                maximum=33,
                const="krw-guru-investigation-brief/v1",
            ),
            "brief_hash": BARE_HASH,
            "research_pack_id": IDENTIFIER,
            "author_key": AUTHOR,
            "ticker": TICKER,
            "company_context_hash": BARE_HASH,
            "questions": array(ref("investigation_question"), 1, 1),
        }
    )
    pack_meta = obj(
        {
            "pack_id": IDENTIFIER,
            "guru_keys": array(AUTHOR, 5, 1, unique=True),
            "question": NONEMPTY,
            "corpus_version": string(maximum=256, minimum=1),
        },
        required=["pack_id", "guru_keys", "question"],
    )
    answerability = obj(
        {
            "direct_source_match": {"type": "boolean"},
            "confidence": string(maximum=6, enum=["low", "medium", "high"]),
            "source_match_strength": string(
                maximum=7,
                enum=["none", "weak", "related", "direct"],
            ),
            "recommended_answer_mode": string(
                maximum=24,
                enum=[
                    "lens_grounded_answer",
                    "lens_with_company_bridge",
                    "clarify_then_answer",
                    "state_ontology_gap",
                ],
            ),
            "reason": string(maximum=4_000, minimum=1),
        }
    )
    intent = obj(
        {
            "family": string(maximum=128, minimum=1),
            "requires_company_evidence": {"type": "boolean"},
            "ticker": TICKER,
        },
        required=["requires_company_evidence"],
    )
    research_pack = obj(
        {
            "format": string(maximum=27, const="krw-guru-research-pack/v1"),
            "research_status": string(maximum=22, enum=RESEARCH_STATUSES),
            "pack_meta": ref("pack_meta"),
            "answerability": ref("answerability"),
            "intent": ref("research_intent"),
            "persona_profile": open_obj(64),
            "philosophy_context": open_obj(128),
            "selected_lenses": array(open_obj(128), 10),
            "consultation_moves": array(open_obj(128), 8),
            "data_needs": array(open_obj(128), 12),
            "company_context": open_obj(128),
            "source_anchors": array(open_obj(64), 8),
            "clarifying_questions": array(NONEMPTY, 8),
            "company_bridge": open_obj(128),
            "trace_recommendations": array(open_obj(64), 16),
            "agent_autonomy": open_obj(64),
            "do_not_call": array(NONEMPTY, 16),
            "warnings": array(NONEMPTY, 32),
        }
    )
    assessment = obj(
        {
            "question_id": QUESTION_ID,
            "evidence_object_ids": array(IDENTIFIER, 24),
            "verdict": string(maximum=10, enum=["mixed", "unresolved"]),
            "reasoning": NONEMPTY,
        }
    )
    analysis = obj(
        {
            "assessments": array(ref("evidence_assessment"), 1, 1),
            "overall_judgment": string(maximum=8_000, minimum=1),
        }
    )
    evidence_source = obj(
        {"object_ids": array(IDENTIFIER, 24, 1, unique=True)}
    )
    evidence_unit = obj(
        {
            "evidence_id": IDENTIFIER,
            "object_id": IDENTIFIER,
            "object_type": string(maximum=256, minimum=1),
            "ticker": TICKER,
            "period": nullable(string(maximum=128, minimum=1)),
            "document_type": nullable(string(maximum=128, minimum=1)),
            "title": nullable(string(maximum=2_000, minimum=1)),
            "summary": nullable(string(maximum=16_000, minimum=1)),
            "directness": nullable(string(maximum=64, minimum=1)),
            "evidence_grade": nullable(string(maximum=64, minimum=1)),
            "source": ref("evidence_source"),
        },
        required=["ticker", "source"],
    )
    research_context = obj(
        {
            "format": string(
                maximum=39,
                const="krw-guru-company-research-context/v1",
            ),
            "ticker": TICKER,
            "brief_hash": BARE_HASH,
            "question_ids": array(QUESTION_ID, 1, 1, unique=True),
            "evidence_units": array(ref("evidence_unit"), 12, 1),
            "source_object_ids": array(IDENTIFIER, 24, 1, unique=True),
            "research_status": string(
                maximum=14,
                enum=["evidence_found", "partial"],
            ),
        }
    )
    decision_question = obj(
        {
            "question_id": QUESTION_ID,
            "decision_role": string(maximum=12, const="main_tension"),
            "question": NONEMPTY,
            "hypothesis": NONEMPTY,
            "counter_hypothesis": NONEMPTY,
            "why_material": NONEMPTY,
            "change_condition": NONEMPTY,
            "verdict": string(maximum=10, enum=["mixed", "unresolved"]),
        }
    )
    decision_frame = obj(
        {
            "main_tension_question_id": QUESTION_ID,
            "questions": array(ref("decision_question"), 1, 1),
        }
    )
    validation = obj(
        {
            "all_questions_assessed": {"type": "boolean", "const": True},
            "evidence_is_question_scoped": {"type": "boolean", "const": True},
            "evidence_mode": string(maximum=10, const="contextual"),
            "allowed_verdicts": {
                "type": "array",
                "prefixItems": [
                    string(maximum=5, const="mixed"),
                    string(maximum=10, const="unresolved"),
                ],
                "items": False,
                "minItems": 2,
                "maxItems": 2,
            },
        }
    )
    validated_analysis = obj(
        {
            "format": string(
                maximum=41,
                const="krw-guru-validated-evidence-analysis/v1",
            ),
            "brief_hash": BARE_HASH,
            "evidence_mode": string(maximum=10, const="contextual"),
            "research_context_hash": BARE_HASH,
            "author_key": AUTHOR,
            "ticker": TICKER,
            "agent_analysis": ref("agent_evidence_analysis"),
            "decision_frame": ref("decision_frame"),
            "validation": ref("evidence_validation"),
        }
    )
    selected_author = obj(
        {
            "author_key": AUTHOR,
            "display_name": string(maximum=256, minimum=1),
        }
    )
    return {
        "json_value": JSON_VALUE,
        "company_context_anchor": anchor,
        "filing_availability": filing_availability,
        "light_company_context": light_context,
        "investigation_question_draft": draft,
        "investigation_question": question,
        "investigation_brief": brief,
        "pack_meta": pack_meta,
        "answerability": answerability,
        "research_intent": intent,
        "research_pack": research_pack,
        "evidence_assessment": assessment,
        "agent_evidence_analysis": analysis,
        "evidence_source": evidence_source,
        "evidence_unit": evidence_unit,
        "company_research_context": research_context,
        "decision_question": decision_question,
        "decision_frame": decision_frame,
        "evidence_validation": validation,
        "validated_evidence_analysis": validated_analysis,
        "selected_author": selected_author,
    }


def query_input() -> dict[str, Any]:
    return obj(
        {
            "question": NONEMPTY,
            "author_keys": array(AUTHOR, 5, 1, unique=True),
            "ticker": TICKER,
            "company_context": ref("light_company_context"),
            "intent_family": string(maximum=128, minimum=1),
            "limit_lens": integer(1, 10),
            "limit_consultation": integer(1, 8),
            "limit_data_needs": integer(1, 12),
        },
        required=["question"],
    )


def query_result() -> dict[str, Any]:
    return obj(
        {
            "research_context_version": string(
                maximum=25,
                const="krw-guru-query-context/v1",
            ),
            "research_status": string(maximum=22, enum=RESEARCH_STATUSES),
            "answerability": ref("answerability"),
            "intent": ref("research_intent"),
            "runtime": open_obj(64),
            "selected_author_keys": array(AUTHOR, 5, 1, unique=True),
            "selected_authors": array(ref("selected_author"), 5, 1),
            "requires_company_evidence": {"type": "boolean"},
            "company_context": open_obj(128),
            "filing_evidence_requirements": array(SHORT_TEXT, 64, unique=True),
            "agent_autonomy": open_obj(64),
            "do_not_call": array(NONEMPTY, 16),
            "research_pack": ref("research_pack"),
            "usage": open_obj(16),
        }
    )


def company_brief_input() -> dict[str, Any]:
    return obj(
        {
            "question": NONEMPTY,
            "author_keys": array(AUTHOR, 5, 1, unique=True),
            "ticker": TICKER,
            "company_name": string(maximum=512, minimum=1),
            "company_context": ref("light_company_context"),
            "guru_query_context": {
                "anyOf": [ref("research_pack"), ref("query_context_result")]
            },
            "investigation_questions": array(
                ref("investigation_question_draft"),
                1,
                1,
            ),
            "intent_family": string(maximum=128, minimum=1),
            "limit_lens": integer(1, 10),
            "limit_consultation": integer(1, 8),
            "limit_data_needs": integer(1, 12),
        },
        required=[
            "question",
            "author_keys",
            "ticker",
            "company_context",
            "guru_query_context",
            "investigation_questions",
        ],
    )


def company_brief_result() -> dict[str, Any]:
    return obj(
        {
            "company_brief_context_version": string(
                maximum=33,
                const="krw-guru-company-brief-context/v1",
            ),
            "research_status": string(maximum=22, enum=RESEARCH_STATUSES),
            "answerability": ref("answerability"),
            "runtime": open_obj(64),
            "selected_author_keys": array(AUTHOR, 5, 1, unique=True),
            "requires_company_evidence": {"type": "boolean"},
            "company_filing_brief": open_obj(128),
            "investigation_brief": nullable(ref("investigation_brief")),
            "next_step": string(maximum=1_000, minimum=1),
            "do_not_call": array(NONEMPTY, 8),
        }
    )


def correction() -> dict[str, Any]:
    return obj(
        {
            "status": string(maximum=25, const="input_correction_required"),
            "code": string(maximum=43, enum=CORRECTION_CODES),
            "message": string(maximum=8_000, minimum=1),
            "required_change": string(maximum=8_000, minimum=1),
            "invalid_fields": array(string(maximum=512, minimum=1), 32, 1),
            "allowed_next_tools": array(
                string(
                    maximum=36,
                    enum=[
                        "krw_guru_company_brief",
                        "krw_guru_review_company_evidence",
                    ],
                ),
                1,
                1,
            ),
            "violations": array(open_obj(64), 32),
        },
        required=[
            "status",
            "code",
            "message",
            "required_change",
            "invalid_fields",
            "allowed_next_tools",
        ],
    )


def review_input() -> dict[str, Any]:
    return obj(
        {
            "question": NONEMPTY,
            "author_keys": array(AUTHOR, 1, 1, unique=True),
            "ticker": TICKER,
            "company_name": string(maximum=512, minimum=1),
            "company_research_context": ref("company_research_context"),
            "investigation_brief": ref("investigation_brief"),
            "agent_analysis": ref("agent_evidence_analysis"),
            "company_context": ref("light_company_context"),
            "intent_family": string(maximum=128, minimum=1),
        },
        required=[
            "question",
            "author_keys",
            "ticker",
            "company_research_context",
            "investigation_brief",
            "agent_analysis",
        ],
    )


def review_result() -> dict[str, Any]:
    return obj(
        {
            "validated_evidence_analysis": ref("validated_evidence_analysis"),
            "usage": obj(
                {
                    "purpose": string(
                        maximum=32,
                        const="validate_main_guru_analysis_only",
                    ),
                    "final_answer": string(
                        maximum=256,
                        const=(
                            "The main Guru writes the final consultation from its "
                            "validated analysis."
                        ),
                    ),
                }
            ),
        }
    )


def build_contracts() -> dict[str, dict[str, Any]]:
    defs = common_defs()
    contracts = {
        "krw-guru-query-context-input/v1": query_input(),
        "krw-guru-query-context-result/v1": query_result(),
        "krw-guru-research-pack/v1": ref("research_pack"),
        "krw-guru-light-company-context/v1": ref("light_company_context"),
        "krw-guru-investigation-question-draft/v1": ref(
            "investigation_question_draft"
        ),
        "krw-guru-company-brief-input/v1": company_brief_input(),
        "krw-guru-investigation-brief/v1": ref("investigation_brief"),
        "krw-guru-company-brief-result/v1": company_brief_result(),
        "krw-guru-input-correction/v1": correction(),
        "krw-guru-company-research-context/v1": ref("company_research_context"),
        "krw-guru-agent-evidence-analysis/v1": ref("agent_evidence_analysis"),
        "krw-guru-evidence-review-input/v1": review_input(),
        "krw-guru-validated-evidence-analysis/v1": ref(
            "validated_evidence_analysis"
        ),
        "krw-guru-evidence-review-result/v1": review_result(),
    }
    return {
        contract_id: root(contract_id, schema, defs)
        for contract_id, schema in contracts.items()
    }


def fixture_values() -> dict[str, Any]:
    light = {
        "format": "krw-guru-light-company-context/v1",
        "ticker": "AAPL",
        "company_name": "Apple Inc.",
        "sector": "Technology",
        "business_description": "Apple designs devices and related services.",
        "primary_activities": ["consumer devices", "digital services"],
        "products_or_segments": ["iPhone", "Services"],
        "revenue_logic": "Device sales and recurring ecosystem services.",
        "context_anchors": [
            {
                "anchor_id": "business_description",
                "kind": "business_description",
                "text": "Apple designs devices and related services.",
            },
            {
                "anchor_id": "segment_services",
                "kind": "segment",
                "text": "Services is a distinct reported segment.",
            },
        ],
        "filing_availability": {
            "has_current_filing": True,
            "has_annual_baseline": True,
        },
    }
    draft = {
        "question": "Does Services support resilience beyond replacement demand?",
        "guru_principle_ids": ["guru:buffett:principle:durable_earnings"],
        "company_context_anchor_ids": [
            "business_description",
            "segment_services",
        ],
        "hypothesis": "Services improves the durability of owner earnings.",
        "counter_hypothesis": "Services remains dependent on replacement demand.",
        "evidence_needed": ["segment disclosure", "revenue driver discussion"],
        "strengthens_if": "Filings show demand resilient to device cycles.",
        "weakens_if": "Filings tie Services economics to device demand.",
        "why_material": "The answer changes whether the business has a durable engine.",
        "decision_role": "main_tension",
    }
    question_identity = {
        "author_key": "buffett",
        "ticker": "AAPL",
        "guru_principle_ids": sorted(draft["guru_principle_ids"]),
        "company_context_anchor_ids": sorted(draft["company_context_anchor_ids"]),
        "decision_role": draft["decision_role"],
        "question": " ".join(draft["question"].casefold().split()),
    }
    question_id = f"q_{bare_sha(question_identity)[:16]}"
    question = {**draft, "question_id": question_id}
    pack = {
        "format": "krw-guru-research-pack/v1",
        "research_status": "needs_company_evidence",
        "pack_meta": {
            "pack_id": "guru-pack-aapl",
            "guru_keys": ["buffett"],
            "question": "Assess Apple through a durable-earnings lens.",
            "corpus_version": "1.0.0",
        },
        "answerability": {
            "direct_source_match": True,
            "confidence": "high",
            "source_match_strength": "direct",
            "recommended_answer_mode": "lens_with_company_bridge",
            "reason": "A company judgment needs filing evidence.",
        },
        "intent": {
            "family": "holding_review",
            "requires_company_evidence": True,
            "ticker": "AAPL",
        },
        "persona_profile": {"source": "guru_ontology"},
        "philosophy_context": {"boundary": "selected-author-inspired"},
        "selected_lenses": [
            {
                "reviewed_id": "guru:buffett:principle:durable_earnings",
                "author_key": "buffett",
                "label_ko": "지속 가능한 수익력",
            }
        ],
        "consultation_moves": [],
        "data_needs": [],
        "company_context": {"ticker": "AAPL"},
        "source_anchors": [],
        "clarifying_questions": [],
        "company_bridge": {"requires_company_evidence": True},
        "trace_recommendations": [],
        "agent_autonomy": {"mode": "company_bridge_required"},
        "do_not_call": ["final company-specific answer before company evidence"],
        "warnings": [],
    }
    query_result_value = {
        "research_context_version": "krw-guru-query-context/v1",
        "research_status": pack["research_status"],
        "answerability": pack["answerability"],
        "intent": pack["intent"],
        "runtime": {"source": "reviewed"},
        "selected_author_keys": ["buffett"],
        "selected_authors": [
            {"author_key": "buffett", "display_name": "Warren Buffett"}
        ],
        "requires_company_evidence": True,
        "company_context": pack["company_context"],
        "filing_evidence_requirements": ["cash_flow"],
        "agent_autonomy": pack["agent_autonomy"],
        "do_not_call": pack["do_not_call"],
        "research_pack": pack,
        "usage": {"default_first_tool": "krw_guru_query_context"},
    }
    # Pydantic's sealing hash uses model_dump(mode="json") without
    # exclude_none, so omitted nullable light-context fields normalize to
    # explicit JSON null before hashing.
    normalized_light_for_seal = {
        **light,
        "sector": light.get("sector"),
        "industry": light.get("industry"),
        "revenue_logic": light.get("revenue_logic"),
    }
    unsigned_brief = {
        "format": "krw-guru-investigation-brief/v1",
        "research_pack_id": pack["pack_meta"]["pack_id"],
        "author_key": "buffett",
        "ticker": "AAPL",
        "company_context_hash": bare_sha(normalized_light_for_seal),
        "questions": [question],
    }
    brief = {**unsigned_brief, "brief_hash": bare_sha(unsigned_brief)}
    runtime_context = {
        "format": "krw-guru-company-research-context/v1",
        "ticker": "AAPL",
        "brief_hash": brief["brief_hash"],
        "question_ids": [question_id],
        "evidence_units": [
            {
                "evidence_id": "ev-services",
                "object_id": "claim:AAPL:services",
                "object_type": "ResearchClaim",
                "ticker": "AAPL",
                "period": "CY2026Q1",
                "document_type": "10-Q",
                "title": "Services",
                "summary": "Services growth is material but device linkage remains.",
                "directness": "direct",
                "evidence_grade": "strong",
                "source": {"object_ids": ["claim:AAPL:services"]},
            }
        ],
        "source_object_ids": ["claim:AAPL:services"],
        "research_status": "partial",
    }
    analysis = {
        "assessments": [
            {
                "question_id": question_id,
                "evidence_object_ids": ["claim:AAPL:services"],
                "verdict": "mixed",
                "reasoning": "The filing context informs but does not settle independence.",
            }
        ],
        "overall_judgment": "The durable-earnings case remains mixed.",
    }
    validated = {
        "format": "krw-guru-validated-evidence-analysis/v1",
        "brief_hash": brief["brief_hash"],
        "evidence_mode": "contextual",
        "research_context_hash": bare_sha(runtime_context),
        "author_key": "buffett",
        "ticker": "AAPL",
        "agent_analysis": analysis,
        "decision_frame": {
            "main_tension_question_id": question_id,
            "questions": [
                {
                    "question_id": question_id,
                    "decision_role": "main_tension",
                    "question": draft["question"],
                    "hypothesis": draft["hypothesis"],
                    "counter_hypothesis": draft["counter_hypothesis"],
                    "why_material": draft["why_material"],
                    "change_condition": draft["weakens_if"],
                    "verdict": "mixed",
                }
            ],
        },
        "validation": {
            "all_questions_assessed": True,
            "evidence_is_question_scoped": True,
            "evidence_mode": "contextual",
            "allowed_verdicts": ["mixed", "unresolved"],
        },
    }
    query_input_value = {
        "question": pack["pack_meta"]["question"],
        "author_keys": ["buffett"],
        "ticker": "AAPL",
        "company_context": light,
        "limit_lens": 4,
        "limit_consultation": 3,
        "limit_data_needs": 5,
    }
    brief_input_value = {
        **query_input_value,
        "company_name": "Apple Inc.",
        "guru_query_context": query_result_value,
        "investigation_questions": [draft],
    }
    company_filing_brief = {
        "format": "krw-guru-company-filing-brief/v1",
        "author_key": "buffett",
        "requires_company_evidence": True,
        "investigation_brief": brief,
    }
    brief_result_value = {
        "company_brief_context_version": "krw-guru-company-brief-context/v1",
        "research_status": pack["research_status"],
        "answerability": pack["answerability"],
        "runtime": query_result_value["runtime"],
        "selected_author_keys": ["buffett"],
        "requires_company_evidence": True,
        "company_filing_brief": company_filing_brief,
        "investigation_brief": brief,
        "next_step": "Pass the company question to filing research.",
        "do_not_call": ["Do not bypass the application orchestrator."],
    }
    review_input_value = {
        "question": pack["pack_meta"]["question"],
        "author_keys": ["buffett"],
        "ticker": "AAPL",
        "company_name": "Apple Inc.",
        "company_research_context": runtime_context,
        "investigation_brief": brief,
        "agent_analysis": analysis,
        "company_context": light,
    }
    review_result_value = {
        "validated_evidence_analysis": validated,
        "usage": {
            "purpose": "validate_main_guru_analysis_only",
            "final_answer": (
                "The main Guru writes the final consultation from its validated analysis."
            ),
        },
    }
    correction_value = {
        "status": "input_correction_required",
        "code": "missing_agent_analysis",
        "message": "agent_analysis is required for the sealed key question.",
        "required_change": "Provide one assessment for the sealed key question.",
        "invalid_fields": ["agent_analysis"],
        "allowed_next_tools": ["krw_guru_review_company_evidence"],
    }
    return {
        "krw-guru-query-context-input/v1": query_input_value,
        "krw-guru-query-context-result/v1": query_result_value,
        "krw-guru-research-pack/v1": pack,
        "krw-guru-light-company-context/v1": light,
        "krw-guru-investigation-question-draft/v1": draft,
        "krw-guru-company-brief-input/v1": brief_input_value,
        "krw-guru-investigation-brief/v1": brief,
        "krw-guru-company-brief-result/v1": brief_result_value,
        "krw-guru-input-correction/v1": correction_value,
        "krw-guru-company-research-context/v1": runtime_context,
        "krw-guru-agent-evidence-analysis/v1": analysis,
        "krw-guru-evidence-review-input/v1": review_input_value,
        "krw-guru-validated-evidence-analysis/v1": validated,
        "krw-guru-evidence-review-result/v1": review_result_value,
    }


def constant_name(contract_id: str) -> str:
    return contract_id.upper().replace("-", "_").replace("/", "_")


def check_pins(root: Path, pins: dict[str, str], label: str) -> dict[str, str]:
    observed: dict[str, str] = {}
    for relative, expected in pins.items():
        digest = hashlib.sha256((root / relative).read_bytes()).hexdigest()
        if digest != expected:
            raise SystemExit(
                f"{label} authority drift: {relative}: expected {expected}, observed {digest}"
            )
        observed[relative] = f"sha256:{digest}"
    return observed


def export(front_root: Path, ontology_root: Path, output_root: Path) -> None:
    front_pins = check_pins(front_root, FRONT_SOURCE_PINS, "front")
    ontology_pins = check_pins(ontology_root, ONTOLOGY_SOURCE_PINS, "ontology")
    contracts = build_contracts()
    fixtures = fixture_values()
    if contracts.keys() != fixtures.keys():
        raise SystemExit("every contract must have exactly one positive fixture")

    schemas_dir = output_root / "schemas"
    schemas_dir.mkdir(parents=True, exist_ok=True)
    manifest_contracts: dict[str, Any] = {}
    bindings = [
        "// @generated by scripts/export_guru_workflow_contracts.py; do not edit."
    ]
    for contract_id, schema in sorted(contracts.items()):
        filename = contract_id.replace("/", "-") + ".json"
        data = canonical(schema)
        (schemas_dir / filename).write_bytes(data)
        digest = sha(data)
        manifest_contracts[contract_id] = {
            "authority_kind": "typescript-host-plus-python-guru-mcp",
            "authority_ref": "default sealed Guru workflow",
            "schema_path": f"schemas/{filename}",
            "schema_sha256": digest,
            "semantic_validation": "bounded_rust_typed_and_cross_artifact_validator",
        }
        name = constant_name(contract_id)
        bindings.append(f'pub const {name}: &str = "{contract_id}";')
        bindings.append(f'pub const {name}_SCHEMA_SHA256: &str = "{digest}";')

    authority = {
        "kind": "krw-guru/default-sealed-workflow-snapshot",
        "front_repository": "krw-ontology-front",
        "front_source_sha256": dict(sorted(front_pins.items())),
        "ontology_repository": "krw-ontology",
        "ontology_source_sha256": dict(sorted(ontology_pins.items())),
        "workflow": [
            "krw_guru_query_context",
            "krw_guru_company_brief",
            "krw_ontology_query_context|krw_ontology_query|krw_ontology_trace",
            "krw_guru_review_company_evidence",
        ],
        "excluded": [
            "krw_guru_company_pack legacy path",
            "krw_ontology_verify_evidence",
        ],
    }
    authority_hash = sha(canonical(authority))
    bundle = {
        "format": FORMAT,
        "json_schema_dialect": DIALECT,
        "authority_sha256": authority_hash,
        "notice": (
            "The pinned TypeScript host and Python Guru MCP remain authoritative; "
            "this is a fail-closed default-workflow projection."
        ),
        "contracts": {key: contracts[key] for key in sorted(contracts)},
    }
    bundle_bytes = canonical(bundle)
    (output_root / "schema-bundle.json").write_bytes(bundle_bytes)

    vectors: list[dict[str, Any]] = []
    for contract_id, value in sorted(fixtures.items()):
        vectors.append(
            {
                "contract_id": contract_id,
                "name": "minimal_valid",
                "valid": True,
                "value": value,
            }
        )
        vectors.append(
            {
                "contract_id": contract_id,
                "name": "reject_unknown_top_level",
                "valid": False,
                "value": {**value, "unexpected": True},
            }
        )
    vectors_document = {
        "format": CONFORMANCE_FORMAT,
        "authority_sha256": authority_hash,
        "vectors": vectors,
    }
    vectors_bytes = canonical(vectors_document)
    (output_root / "conformance-vectors.json").write_bytes(vectors_bytes)

    manifest = {
        "format": FORMAT,
        "authority": authority,
        "authority_sha256": authority_hash,
        "schema_bundle": {
            "path": "schema-bundle.json",
            "sha256": sha(bundle_bytes),
        },
        "conformance_vectors": {
            "path": "conformance-vectors.json",
            "sha256": sha(vectors_bytes),
        },
        "contracts": manifest_contracts,
    }
    manifest_bytes = canonical(manifest)
    (output_root / "manifest.json").write_bytes(manifest_bytes)
    manifest_hash = sha(manifest_bytes)
    (output_root / "manifest.sha256").write_text(
        f"{manifest_hash}\n",
        encoding="utf-8",
    )
    bindings.append(
        f'pub const GURU_GENERATED_MANIFEST_SHA256: &str = "{manifest_hash}";'
    )
    (output_root / "bindings.rs").write_text(
        "\n".join(bindings) + "\n",
        encoding="utf-8",
    )


def artifact_tree(root: Path) -> dict[str, bytes]:
    if root.is_symlink() or not root.is_dir():
        raise SystemExit("contract output root must be a real directory")
    artifacts: dict[str, bytes] = {}
    for path in root.rglob("*"):
        if path.is_symlink():
            raise SystemExit("contract output must not contain symlinks")
        if path.is_file():
            artifacts[path.relative_to(root).as_posix()] = path.read_bytes()
    return artifacts


def check(front_root: Path, ontology_root: Path, output_root: Path) -> None:
    expected = artifact_tree(output_root)
    with tempfile.TemporaryDirectory(prefix="krw-guru-contract-check-") as temporary:
        generated_root = Path(temporary) / "generated"
        export(front_root, ontology_root, generated_root)
        generated = artifact_tree(generated_root)
    if expected.keys() != generated.keys():
        raise SystemExit("Guru contract artifact inventory drift")
    for relative in sorted(expected):
        if expected[relative] != generated[relative]:
            raise SystemExit(f"Guru contract artifact drift: {relative}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--front-root", type=Path, required=True)
    parser.add_argument("--ontology-root", type=Path, required=True)
    parser.add_argument("--output-root", type=Path, required=True)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    front_root = args.front_root.resolve()
    ontology_root = args.ontology_root.resolve()
    output_root = args.output_root.resolve()
    if args.check:
        check(front_root, ontology_root, output_root)
    else:
        export(front_root, ontology_root, output_root)


if __name__ == "__main__":
    main()
