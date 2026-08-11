"""Read-only tool functions for the standalone guru advisor MCP."""

from __future__ import annotations

from collections import Counter
from enum import Enum
from functools import lru_cache
import hashlib
import json
import os
import re
import sqlite3
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence

from krw_capability_runtime.guru.company_bridge import (
    build_guru_company_evidence_review,
    build_guru_company_research_pack,
    company_filing_brief_from_guru_lens,
    validate_guru_agent_evidence_analysis,
)
from krw_capability_runtime.guru.company_context import (
    coerce_company_context,
    company_context_search_text,
    company_context_topics,
)
from krw_capability_runtime.guru.context_taxonomy import (
    contains_context_term,
    context_match_bonus,
    context_mismatch_penalty,
    context_tags_for_text,
    overfit_penalty_rules,
    sector_context_tags,
)
from krw_capability_runtime.guru.index import (
    guru_ranking_text,
    guru_index_status,
    load_guru_index_objects_by_id,
    load_guru_index_relationships_by_id,
    load_guru_index_bundle,
    scan_guru_index_compact_rows,
)
from krw_capability_runtime.guru.models import (
    GURU_RESEARCH_PACK_FORMAT,
    GuruAnswerability,
    GuruResearchIntent,
    GuruResearchPack,
    GuruResearchPackMeta,
)
from krw_capability_runtime.mcp_server.evidence_pack import (
    VERIFIED_COMPANY_EVIDENCE_FORMAT,
    is_verified_company_evidence_pack,
)
from krw_capability_runtime.guru.renderer import build_guru_answer_render_plan
from krw_capability_runtime.guru.workspace import GURU_RUNNING_ROOT_ENV, guru_root


GURU_EVAL_QUESTIONS_PATH_ENV = "KRW_GURU_EVAL_QUESTIONS_PATH"

REVIEWED_FILES = {
    "guru_objects": "guru_objects.jsonl",
    "consultation_objects": "consultation_objects.jsonl",
    "data_needs": "data_needs.jsonl",
    "relationships": "relationships.jsonl",
    "corpus_metadata": "corpus_metadata.jsonl",
    "rejected_candidates": "rejected_candidates.jsonl",
}
OBJECT_FAMILY_BY_FILE = {
    "guru_objects": "guru_object",
    "consultation_objects": "consultation_object",
    "data_needs": "data_need",
    "corpus_metadata": "corpus_metadata",
}
FAMILY_BY_OBJECT_FAMILY = {value: key for key, value in OBJECT_FAMILY_BY_FILE.items()}
AUTHOR_DISPLAY_NAMES = {
    "buffett": "Warren Buffett",
    "marks": "Howard Marks",
    "ackman": "Bill Ackman",
    "flatt": "Bruce Flatt",
    "terry_smith": "Terry Smith",
}
DEFAULT_GURU_SEARCH_FAMILIES = (
    "guru_objects",
    "consultation_objects",
    "data_needs",
)
GURU_FTS_SHADOW_MIN = 64
GURU_FTS_SHADOW_MAX = 512
GURU_FTS_SHADOW_MULTIPLIER = 16
DATA_NEED_THEME_PENALTY_CAP = 6
UNSPECIFIED_GENERAL_DOMAIN_PENALTY_CAP = 4
_WORD_RE = re.compile(r"[0-9A-Za-z가-힣_]+")
_KOREAN_TOKEN_RE = re.compile(r"^[가-힣]+$")
_LOW_INFORMATION_QUERY_TOKENS = {
    "것",
    "거",
    "관점",
    "관점에서",
    "그",
    "때",
    "뭐",
    "무엇",
    "부분",
    "수",
    "어떤",
    "어느",
    "어떻게",
    "왜",
    "없이",
    "있다",
    "있어",
    "있을",
    "회사명",
}
_KOREAN_PARTICLE_SUFFIXES = (
    "에게서",
    "으로부터",
    "에서부터",
    "이라면",
    "라면",
    "으로",
    "에서",
    "에게",
    "부터",
    "까지",
    "처럼",
    "보다",
    "하고",
    "이나",
    "나마",
    "든지",
    "라도",
    "이며",
    "로",
    "을",
    "를",
    "이",
    "가",
    "은",
    "는",
    "의",
    "에",
    "와",
    "과",
    "도",
    "만",
)
_KOREAN_PREDICATE_SUFFIXES = (
    "하다면",
    "하면서",
    "하는데",
    "했는데",
    "한데",
    "해서",
    "하며",
    "하면",
    "하게",
    "하고",
    "하는",
    "했다",
    "하다",
    "한",
    "해",
)
NON_TICKER_ACRONYMS = {
    "ARR",
    "CAC",
    "CAGR",
    "CEO",
    "CFO",
    "DCF",
    "EBIT",
    "EBITDA",
    "EPS",
    "ETF",
    "FCF",
    "GAAP",
    "IRR",
    "KPI",
    "LTV",
    "NAV",
    "PBR",
    "PER",
    "ROA",
    "ROE",
    "ROIC",
    "SOTP",
    "TAM",
}
CONTEXT_OBJECT_TYPE_PRIORITIES = {
    "learn_guru_view": (
        "principle",
        "decision_criterion",
        "heuristic",
        "concept",
        "valuation_frame",
        "time_horizon_frame",
    ),
    "business_quality_check": (
        "principle",
        "decision_criterion",
        "heuristic",
        "concept",
        "time_horizon_frame",
        "anti_pattern",
    ),
    "risk_check": (
        "risk_frame",
        "behavioral_warning",
        "anti_pattern",
        "decision_criterion",
        "heuristic",
    ),
    "cyclical_risk_check": (
        "risk_frame",
        "behavioral_warning",
        "anti_pattern",
        "time_horizon_frame",
        "heuristic",
    ),
    "valuation_check": (
        "valuation_frame",
        "decision_criterion",
        "principle",
        "anti_pattern",
        "heuristic",
    ),
    "capital_allocation_check": (
        "decision_criterion",
        "principle",
        "heuristic",
        "valuation_frame",
        "anti_pattern",
    ),
    "position_sizing": (
        "risk_frame",
        "behavioral_warning",
        "anti_pattern",
        "decision_criterion",
        "heuristic",
    ),
    "holding_review": (
        "decision_criterion",
        "risk_frame",
        "principle",
        "heuristic",
        "behavioral_warning",
    ),
    "sell_or_trim": (
        "risk_frame",
        "behavioral_warning",
        "anti_pattern",
        "decision_criterion",
        "heuristic",
    ),
    "thesis_review": (
        "decision_criterion",
        "heuristic",
        "anti_pattern",
        "valuation_frame",
        "risk_frame",
    ),
    "contrarian_check": (
        "behavioral_warning",
        "anti_pattern",
        "risk_frame",
        "heuristic",
        "principle",
    ),
    "considering_buy": (
        "decision_criterion",
        "principle",
        "heuristic",
        "risk_frame",
        "anti_pattern",
    ),
}
CONSULTATION_OBJECT_TYPE_PRIORITIES = {
    "question_template": 12,
    "answer_playbook": 10,
    "clarifying_question": 8,
    "answer_section": 5,
    "investor_intent": 3,
    "question_route": 2,
}


class GuruResponseFormat(str, Enum):
    """Supported Guru MCP response formats."""

    JSON = "json"
    MARKDOWN = "markdown"


def guru_status_tool(
    *,
    root: str | Path | None = None,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Return reviewed guru ontology availability and quality metadata."""
    root_path = _resolve_root(root)
    reviewed_dir = root_path / "reviewed"
    report_path = reviewed_dir / "curation_report.json"
    report = _read_json(report_path)
    index_status = guru_index_status(root_path)
    files = {
        name: {
            "path": str(reviewed_dir / filename),
            "exists": (reviewed_dir / filename).is_file(),
            "rows": _count_jsonl(reviewed_dir / filename),
        }
        for name, filename in REVIEWED_FILES.items()
    }
    warnings = list(report.get("warnings") or []) if isinstance(report, Mapping) else []
    payload = {
        "ok": reviewed_dir.is_dir() and bool(report) and not warnings,
        "service": "krw_guru_advisor_mcp",
        "root": str(root_path),
        "reviewed_dir": str(reviewed_dir),
        "curation_report_path": str(report_path),
        "curation_report_present": report_path.is_file(),
        "schema_version": report.get("schema_version") if isinstance(report, Mapping) else None,
        "generated_at": report.get("generated_at") if isinstance(report, Mapping) else None,
        "runtime": {
            "mode": index_status["runtime_mode"],
            "index_present": index_status["present"],
            "index_usable": index_status["usable"],
            "index_manifest_path": index_status["manifest_path"],
        },
        "index": index_status,
        "counts": {
            "guru_objects": files["guru_objects"]["rows"],
            "consultation_objects": files["consultation_objects"]["rows"],
            "data_needs": files["data_needs"]["rows"],
            "corpus_metadata": files["corpus_metadata"]["rows"],
            "relationships": files["relationships"]["rows"],
            "rejected_candidates": files["rejected_candidates"]["rows"],
        },
        "warnings": warnings,
        "rejection_reasons": (
            report.get("rejection_reasons") if isinstance(report, Mapping) else {}
        ),
        "files": files,
    }
    if response_format == GuruResponseFormat.MARKDOWN:
        return _status_markdown(payload)
    return _json(payload)


def guru_index_context_tool(
    *,
    root: str | Path | None = None,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Return author-shard index status and MCP runtime policy."""
    root_path = _resolve_root(root)
    status = guru_index_status(root_path)
    payload = {
        "index_context_version": "krw-guru-index-context/v1",
        "root": str(root_path),
        "status": status,
        "mcp_runtime_policy": {
            "default_first_tool": "krw_guru_query_context",
            "preferred_read_path": "author_shard" if status["usable"] else "reviewed_jsonl",
            "fallback": "reviewed_jsonl",
            "source_of_truth": "reviewed_jsonl",
        },
        "optimization": {
            "sharding_key": "author_key",
            "reason": (
                "Guru consultations normally target one persona. Per-author shards avoid "
                "loading unrelated gurus while preserving multi-guru fan-out."
            ),
            "global_index_policy": (
                "Use the manifest as a lightweight global map; do not build a heavy "
                "company-style global graph unless cross-author traversal becomes a "
                "dominant product path."
            ),
        },
    }
    if response_format == GuruResponseFormat.MARKDOWN:
        lines = [
            "# KRW Guru Index",
            "",
            f"- runtime: {status['runtime_mode']}",
            f"- manifest: {status['manifest_path']}",
            f"- usable: {str(status['usable']).lower()}",
            f"- authors: {', '.join(status.get('authors', {}).keys()) or '(none)'}",
        ]
        return "\n".join(lines)
    return _json(payload)


def guru_search_tool(
    *,
    query: str,
    root: str | Path | None = None,
    author_keys: Sequence[str] | None = None,
    object_types: Sequence[str] | None = None,
    intent_family: str | None = None,
    decision_stage: str | None = None,
    requires_company_data: bool | None = None,
    limit: int = 10,
    offset: int = 0,
    include_relationships: bool = False,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Search reviewed guru lens and consultation objects."""
    root_path = _resolve_root(root)
    page_limit = _limit(limit, maximum=50)
    page_offset = max(0, offset)
    required_candidates = page_offset + page_limit
    family_limits = {
        family: _fts_shadow_limit(min(required_candidates, page_limit))
        for family in DEFAULT_GURU_SEARCH_FAMILIES
    }
    bundle = _load_reviewed_bundle(
        root_path,
        author_keys=author_keys,
        query=query,
        family_limits=family_limits,
        object_types=object_types,
        intent_family=_clean_optional(intent_family),
        decision_stage=_clean_optional(decision_stage),
        requires_company_data=requires_company_data,
        projection_profile="search",
    )
    rows = _filter_rows(
        _iter_searchable_rows(bundle, DEFAULT_GURU_SEARCH_FAMILIES),
        author_keys=author_keys,
        object_types=object_types,
        intent_family=intent_family,
        decision_stage=decision_stage,
        requires_company_data=requires_company_data,
    )
    scored = [
        (score, row) for row in rows if (score := _score_row(query, row)) > 0 or not query.strip()
    ]
    scored.sort(
        key=lambda item: (
            -item[0],
            item[1].get("author_key") or "",
            item[1].get("reviewed_id") or "",
        )
    )
    total = len(scored)
    page = scored[page_offset : page_offset + page_limit]
    _hydrate_candidate_payloads(
        root_path,
        bundle,
        [row for _score, row in page],
    )
    _hydrate_candidate_relationships(
        root_path,
        bundle,
        [row for _score, row in page],
    )
    result_rows = [
        _compact_result(
            row,
            score=score,
            relationships=bundle["relationships_by_id"],
            include_relationships=include_relationships,
        )
        for score, row in page
    ]
    payload = {
        "query": query,
        "root": str(root_path),
        "runtime": _bundle_runtime(bundle),
        "total": total,
        "count": len(result_rows),
        "offset": page_offset,
        "limit": page_limit,
        "has_more": total > page_offset + len(result_rows),
        "filters": {
            "author_keys": _clean_list(author_keys),
            "object_types": _clean_list(object_types),
            "intent_family": _clean_optional(intent_family),
            "decision_stage": _clean_optional(decision_stage),
            "requires_company_data": requires_company_data,
        },
        "results": result_rows,
        "usage": {
            "company_facts_policy": (
                "Guru results are investor lenses only. Use KRW Ontology filing research "
                "for named-company facts before company-specific judgment."
            )
        },
    }
    if response_format == GuruResponseFormat.MARKDOWN:
        return _search_markdown(payload)
    return _json(payload)


def guru_query_context_tool(
    *,
    question: str,
    root: str | Path | None = None,
    author_keys: Sequence[str] | None = None,
    ticker: str | None = None,
    company_context: dict[str, Any] | None = None,
    company_context_json: Any = None,
    intent_family: str | None = None,
    limit_lens: int = 4,
    limit_consultation: int = 3,
    limit_data_needs: int = 5,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Return a bounded answer-planning research pack for one guru question."""
    provided_context = _json_arg(
        company_context if company_context is not None else company_context_json,
        field_name="company_context",
    )
    if not provided_context and ticker:
        provided_context = _trusted_light_company_context(ticker)
    payload = _build_guru_research_context(
        question=question,
        root=root,
        author_keys=author_keys,
        ticker=ticker,
        company_context=provided_context,
        intent_family=intent_family,
        limit_lens=limit_lens,
        limit_consultation=limit_consultation,
        limit_data_needs=limit_data_needs,
    )
    if response_format == GuruResponseFormat.MARKDOWN:
        return _query_context_markdown(payload)
    return _json(payload)


def guru_company_brief_tool(
    *,
    question: str,
    root: str | Path | None = None,
    author_keys: Sequence[str] | None = None,
    ticker: str | None = None,
    company_name: str | None = None,
    company_context: dict[str, Any] | None = None,
    company_context_json: Any = None,
    guru_query_context: dict[str, Any] | None = None,
    guru_query_context_json: Any = None,
    investigation_questions: Sequence[Mapping[str, Any]] | None = None,
    intent_family: str | None = None,
    limit_lens: int = 4,
    limit_consultation: int = 3,
    limit_data_needs: int = 5,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Build a company filing brief from selected guru ontology lenses."""
    raw_query_context = (
        guru_query_context
        if guru_query_context is not None
        else guru_query_context_json
    )
    query_payload = (
        _validated_supplied_guru_query_context(
            _json_arg(raw_query_context, field_name="guru_query_context"),
            author_keys=author_keys,
            ticker=ticker,
        )
        if raw_query_context is not None
        else json.loads(
            guru_query_context_tool(
                question=question,
                root=root,
                author_keys=author_keys,
                ticker=ticker,
                company_context_json=(
                    company_context if company_context is not None else company_context_json
                ),
                intent_family=intent_family,
                limit_lens=limit_lens,
                limit_consultation=limit_consultation,
                limit_data_needs=limit_data_needs,
                response_format=GuruResponseFormat.JSON,
            )
        )
    )
    brief = company_filing_brief_from_guru_lens(
        query_payload,
        ticker=ticker,
        company_name=company_name,
        company_context=_json_arg(
            company_context if company_context is not None else company_context_json,
            field_name="company_context",
        ),
        author_key=(author_keys[0] if author_keys else None),
        question=question,
        investigation_questions=investigation_questions,
    )
    brief_payload = brief.model_dump(mode="json", exclude_none=True)
    payload = {
        "company_brief_context_version": "krw-guru-company-brief-context/v1",
        "research_status": query_payload.get("research_status"),
        "answerability": query_payload.get("answerability"),
        "runtime": query_payload.get("runtime"),
        "selected_author_keys": query_payload.get("selected_author_keys"),
        "requires_company_evidence": brief.requires_company_evidence,
        "company_filing_brief": brief_payload,
        "investigation_brief": (
            brief.investigation_brief.model_dump(mode="json", exclude_none=True)
            if brief.investigation_brief is not None
            else None
        ),
        "next_step": _company_brief_next_step(brief_payload),
        "do_not_call": [
            "Do not call broad guru search after this brief unless query_context returned ontology_gap.",
            "Do not let Guru MCP call the company MCP directly; application/orchestrator owns that step.",
        ],
    }
    if response_format == GuruResponseFormat.MARKDOWN:
        return _company_brief_markdown(payload)
    return _json(payload)


def _validated_supplied_guru_query_context(
    payload: Mapping[str, Any],
    *,
    author_keys: Sequence[str] | None,
    ticker: str | None,
) -> dict[str, Any]:
    """Accept the runner-bound research result that selected the draft rules.

    Re-querying here can produce a different ranked set of lens IDs between the
    main Guru's draft and the server's sealing check.  The feature-enabled path
    therefore binds sealing to the original query-context response (or its
    immutable research-pack projection).
    """
    supplied = dict(payload)
    research_pack = supplied.get("research_pack")
    context = supplied
    if not isinstance(research_pack, Mapping) and supplied.get("format") == GURU_RESEARCH_PACK_FORMAT:
        research_pack = supplied
        context = {"research_pack": supplied}
    if not isinstance(research_pack, Mapping):
        raise ValueError(
            "guru_query_context must be the exact krw_guru_query_context result "
            "containing research_pack"
        )
    if research_pack.get("format") != GURU_RESEARCH_PACK_FORMAT:
        raise ValueError("guru_query_context has an invalid Guru ResearchPack format")
    pack_meta = research_pack.get("pack_meta")
    if not isinstance(pack_meta, Mapping):
        raise ValueError("guru_query_context research_pack is missing pack_meta")
    pack_authors = _clean_list(pack_meta.get("guru_keys"))
    requested_authors = _clean_list(author_keys)
    if requested_authors and pack_authors != requested_authors:
        raise ValueError(
            "guru_query_context selected authors do not match author_keys for company brief"
        )
    requested_ticker = _clean_optional(ticker)
    pack_intent = research_pack.get("intent")
    pack_ticker = (
        _clean_optional(pack_intent.get("ticker"))
        if isinstance(pack_intent, Mapping)
        else None
    )
    if requested_ticker and pack_ticker and requested_ticker.upper() != pack_ticker.upper():
        raise ValueError(
            "guru_query_context ticker does not match the requested company brief ticker"
        )
    if not isinstance(research_pack.get("selected_lenses"), Sequence):
        raise ValueError("guru_query_context research_pack is missing selected_lenses")
    return context


def guru_company_pack_tool(
    *,
    question: str,
    root: str | Path | None = None,
    author_keys: Sequence[str] | None = None,
    ticker: str | None = None,
    company_name: str | None = None,
    company_payload: dict[str, Any] | None = None,
    company_payload_json: Any = None,
    company_context: dict[str, Any] | None = None,
    company_context_json: Any = None,
    intent_family: str | None = None,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Build the internal GuruCompanyResearchPack and answer render plan."""
    query_payload = json.loads(
        guru_query_context_tool(
            question=question,
            root=root,
            author_keys=author_keys,
            ticker=ticker,
            company_context_json=(
                company_context if company_context is not None else company_context_json
            ),
            intent_family=intent_family,
            response_format=GuruResponseFormat.JSON,
        )
    )
    company_payload_data = _json_arg(
        company_payload if company_payload is not None else company_payload_json,
        field_name="company_payload",
    )
    company_pack = build_guru_company_research_pack(
        query_payload,
        company_evidence_payload=company_payload_data,
        ticker=ticker,
        company_name=company_name,
        company_context=_json_arg(
            company_context if company_context is not None else company_context_json,
            field_name="company_context",
        ),
        author_key=(author_keys[0] if author_keys else None),
        question=question,
    )
    render_plan = build_guru_answer_render_plan(company_pack)
    payload = {
        "company_pack_context_version": "krw-guru-company-pack-context/v1",
        "research_status": query_payload.get("research_status"),
        "company_pack": company_pack.model_dump(mode="json", exclude_none=True),
        "render_plan": render_plan.model_dump(mode="json", exclude_none=True),
        "usage": {
            "company_mcp_called_by": "application_orchestrator",
            "answer_agent_input": "Use company_pack plus render_plan; do not expose internal payload names.",
        },
    }
    if response_format == GuruResponseFormat.MARKDOWN:
        return _company_pack_markdown(payload)
    return _json(payload)


def guru_review_company_evidence_tool(
    *,
    question: str,
    root: str | Path | None = None,
    author_keys: Sequence[str] | None = None,
    ticker: str | None = None,
    company_name: str | None = None,
    company_payload: dict[str, Any] | None = None,
    company_payload_json: Any = None,
    company_research_context: dict[str, Any] | None = None,
    company_research_context_json: Any = None,
    investigation_brief: dict[str, Any] | None = None,
    investigation_brief_json: Any = None,
    agent_analysis: dict[str, Any] | None = None,
    agent_analysis_json: Any = None,
    company_context: dict[str, Any] | None = None,
    company_context_json: Any = None,
    intent_family: str | None = None,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Validate Guru analysis or run the legacy evidence-review path."""
    raw_investigation_brief = (
        investigation_brief
        if investigation_brief is not None
        else investigation_brief_json
    )
    raw_agent_analysis = (
        agent_analysis if agent_analysis is not None else agent_analysis_json
    )
    investigation_brief_data = (
        _json_arg(raw_investigation_brief, field_name="investigation_brief")
        if raw_investigation_brief is not None
        else None
    )
    agent_analysis_data = (
        _json_arg(raw_agent_analysis, field_name="agent_analysis")
        if raw_agent_analysis is not None
        else None
    )
    if investigation_brief_data is not None or agent_analysis_data is not None:
        if investigation_brief_data is None or agent_analysis_data is None:
            raise ValueError(
                "investigation_brief and agent_analysis must be supplied together"
            )
        research_context_data = _json_arg(
            company_research_context
            if company_research_context is not None
            else company_research_context_json,
            field_name="company_research_context",
        )
        if not research_context_data:
            raise ValueError(
                "company_research_context must be the runtime-built "
                "krw-guru-company-research-context/v1 payload"
            )
        validated = validate_guru_agent_evidence_analysis(
            investigation_brief=investigation_brief_data,
            company_research_context=research_context_data,
            agent_analysis=agent_analysis_data,
        )
        payload = {
            "validated_evidence_analysis": validated.model_dump(
                mode="json", exclude_none=True
            ),
            "usage": {
                "purpose": "validate_main_guru_analysis_only",
                "final_answer": "The main Guru writes the final consultation from its validated analysis.",
            },
        }
        if response_format == GuruResponseFormat.MARKDOWN:
            return _json(payload)
        return _json(payload)

    company_payload_data = _json_arg(
        company_payload if company_payload is not None else company_payload_json,
        field_name="company_payload",
    )
    if not is_verified_company_evidence_pack(company_payload_data):
        raise ValueError(
            "company_payload must be an unmodified "
            f"{VERIFIED_COMPANY_EVIDENCE_FORMAT} payload returned by "
            "krw_ontology_verify_evidence"
        )
    normalized_ticker = str(ticker or "").strip().upper()
    payload_ticker = str(company_payload_data.get("ticker") or "").strip().upper()
    if normalized_ticker and payload_ticker != normalized_ticker:
        raise ValueError("company_payload ticker does not match the requested ticker")
    query_payload = json.loads(
        guru_query_context_tool(
            question=question,
            root=root,
            author_keys=author_keys,
            ticker=ticker,
            company_context_json=(
                company_context if company_context is not None else company_context_json
            ),
            intent_family=intent_family,
            response_format=GuruResponseFormat.JSON,
        )
    )
    review = build_guru_company_evidence_review(
        query_payload,
        company_evidence_payload=company_payload_data,
        ticker=ticker,
        company_name=company_name,
        company_context=_json_arg(
            company_context if company_context is not None else company_context_json,
            field_name="company_context",
        ),
        author_key=(author_keys[0] if author_keys else None),
        question=question,
    )
    payload = {
        "company_evidence_review_context_version": "krw-guru-company-evidence-review-context/v1",
        "research_status": query_payload.get("research_status"),
        "company_evidence_review": review.model_dump(mode="json", exclude_none=True),
        "usage": {
            "company_mcp_called_by": "application_orchestrator",
            "answer_agent_input": (
                "Use this review as interpretation guidance with Guru ResearchPack and "
                "CompanyEvidencePack; do not expose internal payload names or fixed sections."
            ),
        },
    }
    if response_format == GuruResponseFormat.MARKDOWN:
        return _company_evidence_review_markdown(payload)
    return _json(payload)


def guru_context_tool(
    *,
    question: str,
    root: str | Path | None = None,
    author_keys: Sequence[str] | None = None,
    ticker: str | None = None,
    intent_family: str | None = None,
    limit_lens: int = 6,
    limit_consultation: int = 4,
    limit_data_needs: int = 8,
    include_relationships: bool = False,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Build a compact guru consultation context pack for an investor question."""
    root_path = _resolve_root(root)
    requested_authors = _clean_list(author_keys)
    selected_authors = requested_authors or _infer_author_keys(question)
    if not selected_authors:
        selected_authors = _default_authors_for_question(question)
    inferred_intent = _clean_optional(intent_family) or _infer_intent_family(question)
    lens_limit = _limit(limit_lens, maximum=20)
    consultation_limit = _limit(limit_consultation, maximum=20)
    data_need_limit = _limit(limit_data_needs, maximum=30)
    bundle = _load_reviewed_bundle(
        root_path,
        author_keys=selected_authors,
        query=question,
        family_limits={
            "guru_objects": _fts_shadow_limit(lens_limit),
            "consultation_objects": _fts_shadow_limit(consultation_limit),
            "data_needs": _fts_shadow_limit(data_need_limit),
        },
    )

    lens_rows = _rank_and_take(
        query=question,
        rows=_filter_rows(
            bundle["guru_objects"],
            author_keys=selected_authors,
            intent_family=inferred_intent,
        ),
        limit=lens_limit,
    )
    if len(lens_rows) < lens_limit:
        extra_lens = _rank_and_take(
            query=question,
            rows=[
                row
                for row in _filter_rows(bundle["guru_objects"], author_keys=selected_authors)
                if row not in lens_rows
            ],
            limit=lens_limit - len(lens_rows),
        )
        lens_rows.extend(extra_lens)

    consultation_rows = _rank_and_take(
        query=question,
        rows=_filter_rows(
            bundle["consultation_objects"],
            author_keys=selected_authors,
            intent_family=inferred_intent,
        ),
        limit=consultation_limit,
    )
    if not consultation_rows:
        consultation_rows = _rank_and_take(
            query=question,
            rows=_filter_rows(bundle["consultation_objects"], author_keys=selected_authors),
            limit=consultation_limit,
        )

    unresolved_asset_wrapper = _question_has_unresolved_asset_wrapper_ambiguity(question, ticker)
    explicit_no_company = (
        _question_denies_company_subject(question) or _question_is_generic_lens_framework(question)
    ) and not _clean_optional(ticker)
    needs_company_data = (
        False
        if explicit_no_company or unresolved_asset_wrapper
        else bool(_clean_optional(ticker)) or _question_mentions_company_need(question)
    )
    if (
        not explicit_no_company
        and not unresolved_asset_wrapper
        and any(row.get("requires_company_data") for row in consultation_rows)
    ):
        needs_company_data = True
    data_needs = _rank_and_take(
        query=question,
        rows=_filter_rows(bundle["data_needs"], author_keys=selected_authors),
        limit=data_need_limit,
    )
    if needs_company_data:
        company_data_needs = [
            row
            for row in data_needs
            if row.get("requires_company_data") or row.get("company_data_hooks")
        ]
        other_data_needs = [row for row in data_needs if row not in company_data_needs]
        data_needs = (company_data_needs + other_data_needs)[:data_need_limit]

    selected_rows = [*lens_rows, *consultation_rows, *data_needs]
    _hydrate_candidate_payloads(root_path, bundle, selected_rows)
    _hydrate_candidate_relationships(root_path, bundle, selected_rows)
    context_quality = _context_quality(
        question=question,
        selected_rows=selected_rows,
    )
    payload = {
        "context_pack_version": "krw-guru-context/v2",
        "question": question,
        "ticker": _clean_optional(ticker),
        "root": str(root_path),
        "runtime": _bundle_runtime(bundle),
        "selected_author_keys": selected_authors,
        "selected_authors": [
            {"author_key": key, "display_name": AUTHOR_DISPLAY_NAMES.get(key, key)}
            for key in selected_authors
        ],
        "intent_family": inferred_intent,
        "context_quality": context_quality,
        "clarifying_questions": _clarifying_questions_for_question(
            question,
            needs_company_data,
            ticker=ticker,
        ),
        "requires_company_evidence": needs_company_data,
        "filing_evidence_requirements": (
            _generic_filing_requirements(question, inferred_intent) if needs_company_data else []
        ),
        "company_evidence_policy": {
            "company_facts_source": "KRW Ontology filing research",
            "guru_facts_boundary": "Guru ontology supplies lenses and questions, not company facts.",
            "required_when": [
                "named company or ticker",
                "buy/add/hold/trim/sell judgment",
                "cash flow, capital allocation, balance sheet, risk, valuation, or segment claims",
            ],
        },
        "lens_objects": [
            _compact_result(
                row,
                score=_score_row(question, row),
                relationships=bundle["relationships_by_id"],
                include_relationships=include_relationships,
            )
            for row in lens_rows
        ],
        "consultation_objects": [
            _compact_result(
                row,
                score=_score_row(question, row),
                relationships=bundle["relationships_by_id"],
                include_relationships=include_relationships,
            )
            for row in consultation_rows
        ],
        "data_needs": [
            _compact_result(
                row,
                score=_score_row(question, row),
                relationships=bundle["relationships_by_id"],
                include_relationships=include_relationships,
            )
            for row in data_needs
        ],
        "tool_guidance": {
            "default_next_step": (
                "Use krw_guru_evidence on one or two reviewed_ids only when stronger source support is needed."
            ),
            "avoid": [
                "Do not treat relationship IDs as user-facing evidence.",
                "Do not infer a direct guru view when context_quality.direct_source_match is false.",
            ],
        },
        "answer_contract": {
            "do": [
                "separate filing-supported facts from guru-derived interpretation",
                "frame decision conditions instead of issuing a buy/sell order",
                "state missing company or portfolio evidence when needed",
            ],
            "do_not": [
                "claim a real guru would buy or sell today",
                "invent company facts from guru ontology",
                "expose raw object IDs unless the user asks for debug details",
            ],
        },
    }
    research_context = _build_guru_research_context(
        question=question,
        root=root_path,
        author_keys=selected_authors,
        ticker=ticker,
        company_context=None,
        intent_family=inferred_intent,
        limit_lens=limit_lens,
        limit_consultation=limit_consultation,
        limit_data_needs=limit_data_needs,
    )
    payload.update(
        {
            "research_context_version": research_context["research_context_version"],
            "research_status": research_context["research_status"],
            "answerability": research_context["answerability"],
            "agent_autonomy": research_context["agent_autonomy"],
            "do_not_call": research_context["do_not_call"],
            "research_pack": research_context["research_pack"],
        }
    )
    if response_format == GuruResponseFormat.MARKDOWN:
        return _context_markdown(payload)
    return _json(payload)


def guru_evidence_tool(
    *,
    reviewed_id: str,
    root: str | Path | None = None,
    include_related: bool = True,
    include_span_metadata: bool = True,
    include_private_excerpt: bool = False,
    max_excerpt_words: int = 25,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Return one reviewed guru object with relationships and source support metadata."""
    root_path = _resolve_root(root)
    bundle = _load_point_reviewed_bundle(
        root_path,
        reviewed_id=reviewed_id,
        include_related=include_related,
        include_neighbor_relationships=False,
        related_limit=None,
    )
    object_row = bundle["objects_by_id"].get(reviewed_id)
    if object_row is None:
        return _json(
            {
                "ok": False,
                "error": "reviewed_id_not_found",
                "reviewed_id": reviewed_id,
                "suggestion": "Call krw_guru_search first and pass a returned reviewed_id.",
            }
        )
    relationships = bundle["relationships_by_id"].get(reviewed_id, []) if include_related else []
    related_objects = []
    if include_related:
        for relationship in relationships:
            other_id = (
                relationship["to_id"]
                if relationship.get("from_id") == reviewed_id
                else relationship.get("from_id")
            )
            if other_id and other_id in bundle["objects_by_id"]:
                related_objects.append(_public_object(bundle["objects_by_id"][other_id]))

    span_metadata = []
    if include_span_metadata:
        span_index = _load_span_index_from_report(root_path)
        for span_id in object_row.get("supporting_span_ids") or []:
            metadata = span_index.get(span_id, {"span_id": span_id, "available": False})
            if include_private_excerpt and metadata.get("text"):
                metadata["excerpt_text"] = _limited_words(
                    str(metadata.pop("text")),
                    max_words=max(1, min(max_excerpt_words, 25)),
                )
            else:
                metadata.pop("text", None)
            span_metadata.append(metadata)

    payload = {
        "ok": True,
        "root": str(root_path),
        "object": _public_object(object_row),
        "relationships": relationships,
        "related_objects": related_objects,
        "supporting_spans": span_metadata,
        "rights_policy": {
            "default": "official_link_only_no_fulltext",
            "private_excerpt_included": include_private_excerpt,
            "max_excerpt_words": max(1, min(max_excerpt_words, 25))
            if include_private_excerpt
            else 0,
        },
    }
    if response_format == GuruResponseFormat.MARKDOWN:
        return _evidence_markdown(payload)
    return _json(payload)


def guru_trace_tool(
    *,
    reviewed_id: str,
    root: str | Path | None = None,
    include_related: bool = True,
    include_span_metadata: bool = True,
    include_private_excerpt: bool = False,
    max_excerpt_words: int = 25,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Trace one guru ontology object to source support and bounded neighbors."""
    return guru_evidence_tool(
        reviewed_id=reviewed_id,
        root=root,
        include_related=include_related,
        include_span_metadata=include_span_metadata,
        include_private_excerpt=include_private_excerpt,
        max_excerpt_words=max_excerpt_words,
        response_format=response_format,
    )


def guru_chain_tool(
    *,
    reviewed_id: str,
    root: str | Path | None = None,
    max_neighbors: int = 8,
    include_relationships: bool = False,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Return bounded ontology neighbors around one selected guru object."""
    root_path = _resolve_root(root)
    bundle = _load_point_reviewed_bundle(
        root_path,
        reviewed_id=reviewed_id,
        include_related=True,
        include_neighbor_relationships=True,
        related_limit=_limit(max_neighbors, maximum=20),
    )
    object_row = bundle["objects_by_id"].get(reviewed_id)
    if object_row is None:
        return _json(
            {
                "ok": False,
                "error": "reviewed_id_not_found",
                "reviewed_id": reviewed_id,
                "suggestion": "Call krw_guru_query_context first and pass a returned reviewed_id.",
            }
        )

    relationships = bundle["relationships_by_id"].get(reviewed_id, [])
    neighbors: list[dict[str, Any]] = []
    edge_paths: list[dict[str, Any]] = []
    for relationship in relationships[: _limit(max_neighbors, maximum=20)]:
        other_id = (
            relationship.get("to_id")
            if relationship.get("from_id") == reviewed_id
            else relationship.get("from_id")
        )
        if not other_id or other_id not in bundle["objects_by_id"]:
            continue
        other = bundle["objects_by_id"][str(other_id)]
        neighbors.append(
            _compact_result(
                other,
                score=_score_row(str(object_row.get("label_ko") or ""), other),
                relationships=bundle["relationships_by_id"],
                include_relationships=False,
            )
        )
        edge_paths.append(
            {
                "relation_type": relationship.get("relation_type"),
                "from_id": relationship.get("from_id"),
                "to_id": relationship.get("to_id"),
                "explanation_ko": relationship.get("explanation_ko"),
            }
        )

    payload = {
        "ok": True,
        "chain_version": "krw-guru-chain/v1",
        "root": str(root_path),
        "object": _public_object(object_row),
        "chain": {
            "semantic_neighbors": neighbors,
            "edge_paths": edge_paths,
            "source_anchors": _source_anchors_for_rows(root_path, [object_row], limit=6),
            "relationship_summary": _relationship_summary(relationships),
        },
        "quality": {
            "evidence_grade": _evidence_grade_for_rows([object_row]),
            "warnings": (
                []
                if object_row.get("supporting_span_ids")
                else ["Selected object has no direct supporting_span_ids."]
            ),
        },
        "usage": {
            "default_next_step": "Use these neighbors only to enrich a selected lens, not to restart broad search."
        },
    }
    if include_relationships:
        payload["relationships"] = relationships[: _limit(max_neighbors, maximum=20)]
    if response_format == GuruResponseFormat.MARKDOWN:
        return _chain_markdown(payload)
    return _json(payload)


def guru_data_needs_tool(
    *,
    question: str,
    root: str | Path | None = None,
    author_keys: Sequence[str] | None = None,
    intent_family: str | None = None,
    limit: int = 10,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Return guru-derived data needs for bridging a consultation to filing research."""
    root_path = _resolve_root(root)
    selected_authors = _clean_list(author_keys) or _infer_author_keys(question)
    inferred_intent = _clean_optional(intent_family) or _infer_intent_family(question)
    result_limit = _limit(limit, maximum=30)
    bundle = _load_reviewed_bundle(
        root_path,
        author_keys=selected_authors or None,
        query=question,
        family_limits={"data_needs": _fts_shadow_limit(result_limit)},
    )
    rows = _filter_rows(
        bundle["data_needs"],
        author_keys=selected_authors or None,
        intent_family=inferred_intent,
    )
    if not rows:
        rows = _filter_rows(bundle["data_needs"], author_keys=selected_authors or None)
    ranked = _rank_and_take(query=question, rows=rows, limit=result_limit)
    _hydrate_candidate_payloads(root_path, bundle, ranked)
    _hydrate_candidate_relationships(root_path, bundle, ranked)
    payload = {
        "question": question,
        "root": str(root_path),
        "runtime": _bundle_runtime(bundle),
        "selected_author_keys": selected_authors,
        "intent_family": inferred_intent,
        "requires_company_evidence": _question_mentions_company_need(question),
        "count": len(ranked),
        "data_needs": [
            _compact_result(
                row, score=_score_row(question, row), relationships=bundle["relationships_by_id"]
            )
            for row in ranked
        ],
        "filing_research_bridge": _filing_bridge_payload(
            question,
            ranked,
            intent_family=inferred_intent,
        ),
    }
    if response_format == GuruResponseFormat.MARKDOWN:
        return _data_needs_markdown(payload)
    return _json(payload)


def guru_eval_questions_tool(
    *,
    lens: str | None = None,
    requires_company_evidence: bool | None = None,
    intent: str | None = None,
    limit: int = 25,
    offset: int = 0,
    response_format: GuruResponseFormat = GuruResponseFormat.JSON,
) -> str:
    """Return curated investor-question eval cases for the guru advisor workflow."""
    rows = _load_eval_questions()
    lens_key = _clean_optional(lens)
    intent_key = _clean_optional(intent)
    if lens_key:
        rows = [row for row in rows if lens_key in row.get("lenses", [])]
    if requires_company_evidence is not None:
        rows = [
            row
            for row in rows
            if bool(row.get("requires_company_evidence")) == requires_company_evidence
        ]
    if intent_key:
        rows = [row for row in rows if row.get("intent") == intent_key]
    total = len(rows)
    page = rows[max(0, offset) : max(0, offset) + _limit(limit, maximum=50)]
    payload = {
        "total": total,
        "count": len(page),
        "offset": max(0, offset),
        "limit": _limit(limit, maximum=50),
        "has_more": total > max(0, offset) + len(page),
        "filters": {
            "lens": lens_key,
            "requires_company_evidence": requires_company_evidence,
            "intent": intent_key,
        },
        "questions": page,
        "coverage": _eval_coverage(rows),
    }
    if response_format == GuruResponseFormat.MARKDOWN:
        return _eval_markdown(payload)
    return _json(payload)


def _build_guru_research_context(
    *,
    question: str,
    root: str | Path | None,
    author_keys: Sequence[str] | None,
    ticker: str | None,
    company_context: Mapping[str, Any] | None,
    intent_family: str | None,
    limit_lens: int,
    limit_consultation: int,
    limit_data_needs: int,
) -> dict[str, Any]:
    root_path = _resolve_root(root)
    selected_authors = _selected_author_keys(question, author_keys)
    inferred_intent = _clean_optional(intent_family) or _infer_intent_family(question)
    company_context_model = coerce_company_context(
        company_context or None,
        ticker=ticker,
    )
    lens_limit = _limit(limit_lens, maximum=10)
    if len(selected_authors) > 1:
        lens_limit = max(lens_limit, min(len(selected_authors), 5))
    consultation_limit = _limit(limit_consultation, maximum=8)
    data_need_limit = _limit(limit_data_needs, maximum=12)
    scoring_query = _scoring_query(question, ticker, company_context_model)
    bundle = _load_reviewed_bundle(
        root_path,
        author_keys=selected_authors,
        query=scoring_query,
        family_limits={
            "guru_objects": _fts_shadow_limit(lens_limit),
            "consultation_objects": _fts_shadow_limit(consultation_limit),
            "data_needs": _fts_shadow_limit(data_need_limit),
        },
    )

    lens_rows = _rank_context_rows(
        query=scoring_query,
        rows=bundle["guru_objects"],
        author_keys=selected_authors,
        intent_family=inferred_intent,
        row_family="guru_object",
        limit=lens_limit,
    )
    consultation_rows = _rank_context_rows(
        query=scoring_query,
        rows=bundle["consultation_objects"],
        author_keys=selected_authors,
        intent_family=inferred_intent,
        row_family="consultation_object",
        limit=consultation_limit,
    )
    unresolved_asset_wrapper = _question_has_unresolved_asset_wrapper_ambiguity(question, ticker)
    explicit_no_company = (
        _question_denies_company_subject(question) or _question_is_generic_lens_framework(question)
    ) and not _clean_optional(ticker)
    needs_company_data = (
        False
        if explicit_no_company or unresolved_asset_wrapper
        else bool(_clean_optional(ticker)) or _question_mentions_company_need(question)
    )
    if (
        not explicit_no_company
        and not unresolved_asset_wrapper
        and any(row.get("requires_company_data") for row in consultation_rows)
    ):
        needs_company_data = True

    data_needs = _rank_context_rows(
        query=scoring_query,
        rows=bundle["data_needs"],
        author_keys=selected_authors,
        intent_family=inferred_intent,
        row_family="data_need",
        limit=data_need_limit,
    )
    if needs_company_data:
        data_needs = _prioritize_company_data_needs(data_needs, data_need_limit)

    selected_rows = [*lens_rows, *consultation_rows, *data_needs]
    _hydrate_candidate_payloads(root_path, bundle, selected_rows)
    _hydrate_candidate_relationships(root_path, bundle, selected_rows)
    context_quality = _context_quality(
        question=question,
        selected_rows=selected_rows,
        intent_family=inferred_intent,
    )
    clarifying_questions = _clarifying_questions_for_question(
        question,
        needs_company_data,
        ticker=ticker,
    )
    answerability = _answerability_from_context(
        context_quality=context_quality,
        needs_company_data=needs_company_data,
        clarifying_questions=clarifying_questions,
        selected_rows=selected_rows,
    )
    research_status = _research_status_from_answerability(
        answerability=answerability,
        needs_company_data=needs_company_data,
        clarifying_questions=clarifying_questions,
        selected_rows=selected_rows,
    )
    report = _read_json(root_path / "reviewed" / "curation_report.json")

    selected_lenses = [
        _compact_result(
            row,
            score=_context_score(
                scoring_query,
                row,
                intent_family=inferred_intent,
                row_family="guru_object",
            ),
            relationships=bundle["relationships_by_id"],
            include_relationships=False,
        )
        for row in lens_rows
    ]
    consultation_moves = [
        _compact_result(
            row,
            score=_context_score(
                scoring_query,
                row,
                intent_family=inferred_intent,
                row_family="consultation_object",
            ),
            relationships=bundle["relationships_by_id"],
            include_relationships=False,
        )
        for row in consultation_rows
    ]
    compact_data_needs = [
        _compact_result(
            row,
            score=_context_score(
                scoring_query,
                row,
                intent_family=inferred_intent,
                row_family="data_need",
            ),
            relationships=bundle["relationships_by_id"],
            include_relationships=False,
        )
        for row in data_needs
    ]
    filing_bridge = _filing_bridge_payload(
        question,
        data_needs,
        intent_family=inferred_intent,
        company_context=company_context_model,
    )
    company_context_payload = (
        company_context_model.model_dump(mode="json", exclude_none=True)
        if company_context_model is not None
        else {}
    )
    light_company_context_payload = _light_company_context_payload(company_context)
    company_bridge = {
        **filing_bridge,
        "requires_company_evidence": needs_company_data,
        "company_facts_source": "KRW Ontology filing research",
        "company_context": company_context_payload,
        "boundary": "Guru MCP supplies lens context only; company facts require filing evidence.",
    }
    pack = GuruResearchPack(
        research_status=research_status,
        pack_meta=GuruResearchPackMeta(
            pack_id=_research_pack_id(question, selected_authors, ticker),
            guru_keys=selected_authors,
            question=question,
            corpus_version=report.get("schema_version") if isinstance(report, Mapping) else None,
        ),
        answerability=GuruAnswerability(**answerability),
        intent=GuruResearchIntent(
            family=inferred_intent,
            requires_company_evidence=needs_company_data,
            ticker=_clean_optional(ticker),
        ),
        persona_profile=_persona_profile_from_rows(selected_authors, selected_rows),
        philosophy_context=_philosophy_context_from_lenses(
            selected_authors=selected_authors,
            lens_rows=lens_rows,
            compact_lenses=selected_lenses,
        ),
        selected_lenses=selected_lenses,
        consultation_moves=consultation_moves,
        data_needs=compact_data_needs,
        company_context=company_context_payload,
        source_anchors=_source_anchors_for_rows(root_path, selected_rows, limit=8),
        clarifying_questions=clarifying_questions,
        company_bridge=company_bridge,
        trace_recommendations=_trace_recommendations(question, lens_rows, consultation_rows),
        agent_autonomy=_guru_agent_autonomy(research_status, needs_company_data),
        do_not_call=_guru_do_not_call(
            research_status,
            needs_company_data=needs_company_data,
        ),
        warnings=_research_pack_warnings(context_quality, selected_rows),
    )
    pack_dict = pack.model_dump(mode="json", exclude_none=True)
    return {
        "research_context_version": "krw-guru-query-context/v1",
        "research_status": pack_dict["research_status"],
        "answerability": pack_dict["answerability"],
        "intent": pack_dict["intent"],
        "runtime": _bundle_runtime(bundle),
        "selected_author_keys": selected_authors,
        "selected_authors": [
            {"author_key": key, "display_name": AUTHOR_DISPLAY_NAMES.get(key, key)}
            for key in selected_authors
        ],
        "requires_company_evidence": needs_company_data,
        # The top-level result field is the strict neutral light context used
        # for kernel linkage. The ResearchPack keeps the richer derived topic
        # projection above for lens ranking and remains generic by contract.
        "company_context": light_company_context_payload,
        "filing_evidence_requirements": (
            _filing_requirements_with_company_context(
                question,
                inferred_intent,
                company_context_model,
            )
            if needs_company_data
            else []
        ),
        "agent_autonomy": pack_dict["agent_autonomy"],
        "do_not_call": pack_dict["do_not_call"],
        "research_pack": pack_dict,
        "usage": {
            "default_first_tool": "krw_guru_query_context",
            "normal_follow_up": (
                "For company-specific questions, call krw_guru_company_brief, "
                "then the app-provided company_evidence_researcher Agent, then "
                "krw_guru_review_company_evidence before final writing. Use "
                "krw_guru_trace or krw_guru_chain only for extra guru-source "
                "support after the company evidence path is satisfied."
                if needs_company_data
                else "Use krw_guru_trace or krw_guru_chain on selected reviewed_ids only."
            ),
            "search_policy": "Use krw_guru_search only for fallback discovery or debugging.",
        },
    }


def _trusted_light_company_context(ticker: str) -> dict[str, Any]:
    """Read neutral orientation through the configured ontology release only."""
    from krw_capability_runtime.mcp_server.tools import trusted_guru_light_company_context

    return trusted_guru_light_company_context(ticker=ticker)


def _light_company_context_payload(value: Mapping[str, Any] | None) -> dict[str, Any]:
    """Keep only the strict neutral context at the query-result boundary."""
    if not isinstance(value, Mapping):
        return {}
    if value.get("format") != "krw-guru-light-company-context/v1":
        return {}
    return dict(value)


def _selected_author_keys(question: str, author_keys: Sequence[str] | None) -> list[str]:
    requested = _valid_author_keys(_clean_list(author_keys))
    if requested:
        return requested
    inferred = _valid_author_keys(_infer_author_keys(question))
    if inferred:
        return inferred
    return _valid_author_keys(_default_authors_for_question(question))


def _valid_author_keys(author_keys: Sequence[str]) -> list[str]:
    valid = []
    for key in author_keys:
        normalized = str(key).strip()
        if normalized in AUTHOR_DISPLAY_NAMES and normalized not in valid:
            valid.append(normalized)
    return valid


def _rank_context_rows(
    *,
    query: str,
    rows: Sequence[dict[str, Any]],
    author_keys: Sequence[str],
    intent_family: str | None,
    row_family: str,
    limit: int,
) -> list[dict[str, Any]]:
    candidates = _filter_rows(rows, author_keys=author_keys)
    scored = [
        (
            _context_score(
                query,
                row,
                intent_family=intent_family,
                row_family=row_family,
            ),
            row,
        )
        for row in candidates
    ]
    scored.sort(
        key=lambda item: (
            -item[0],
            item[1].get("author_key") or "",
            item[1].get("reviewed_id") or "",
        )
    )
    if len(author_keys) <= 1:
        return [row for score, row in scored if score > 0][:limit]

    selected: list[dict[str, Any]] = []
    selected_ids: set[str] = set()
    for author_key in author_keys:
        for score, row in scored:
            reviewed_id = str(row.get("reviewed_id") or id(row))
            if reviewed_id in selected_ids or row.get("author_key") != author_key or score <= 0:
                continue
            selected.append(row)
            selected_ids.add(reviewed_id)
            break
        if len(selected) >= limit:
            return selected
    for score, row in scored:
        reviewed_id = str(row.get("reviewed_id") or id(row))
        if reviewed_id in selected_ids or score <= 0:
            continue
        selected.append(row)
        selected_ids.add(reviewed_id)
        if len(selected) >= limit:
            break
    return selected


def _prioritize_company_data_needs(
    rows: Sequence[dict[str, Any]],
    limit: int,
) -> list[dict[str, Any]]:
    company_rows = [
        row for row in rows if row.get("requires_company_data") or row.get("company_data_hooks")
    ]
    other_rows = [row for row in rows if row not in company_rows]
    return [*company_rows, *other_rows][:limit]


def _research_pack_id(question: str, author_keys: Sequence[str], ticker: str | None) -> str:
    raw = json.dumps(
        {"question": question, "author_keys": list(author_keys), "ticker": ticker},
        ensure_ascii=False,
        sort_keys=True,
    )
    return "guru_pack:" + hashlib.sha1(raw.encode("utf-8")).hexdigest()[:16]


def _context_quality(
    *,
    question: str,
    selected_rows: Sequence[Mapping[str, Any]],
    intent_family: str | None = None,
) -> dict[str, Any]:
    scored = [
        (
            _context_score(
                question,
                row,
                intent_family=intent_family,
                row_family=_row_family(row),
            ),
            row,
        )
        for row in selected_rows
    ]
    positive = [(score, row) for score, row in scored if score > 0]
    top_score = max((score for score, _row in positive), default=0)
    source_grounded = [
        row
        for score, row in positive
        if score > 0 and row.get("object_origin") == "source_grounded"
    ]
    span_supported = [
        row for score, row in positive if score > 0 and row.get("supporting_span_ids")
    ]
    evidence_positive = [
        (score, row)
        for score, row in positive
        if row.get("object_origin") in {"source_grounded", "corpus_synthesized"}
        or row.get("supporting_span_ids")
    ]
    top_evidence_score = max((score for score, _row in evidence_positive), default=0)
    if top_evidence_score >= 20 and span_supported:
        strength = "direct"
        direct_source_match = True
        confidence = "high"
    elif top_evidence_score >= 12 and (span_supported or source_grounded):
        strength = "related"
        direct_source_match = False
        confidence = "medium"
    elif positive:
        strength = "weak"
        direct_source_match = False
        confidence = "low"
    else:
        strength = "none"
        direct_source_match = False
        confidence = "low"
    return {
        "top_score": top_score,
        "top_evidence_score": top_evidence_score,
        "matched_rows": len(positive),
        "source_grounded_rows": len(source_grounded),
        "span_supported_rows": len(span_supported),
        "source_match_strength": strength,
        "direct_source_match": direct_source_match,
        "confidence": confidence,
    }


def _answerability_from_context(
    *,
    context_quality: Mapping[str, Any],
    needs_company_data: bool,
    clarifying_questions: Sequence[str],
    selected_rows: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    if not selected_rows:
        return {
            "direct_source_match": False,
            "confidence": "low",
            "source_match_strength": "none",
            "recommended_answer_mode": "state_ontology_gap",
            "reason": "No reviewed guru ontology objects matched the question.",
        }
    if _clarification_blocks_strong_answer(clarifying_questions):
        return {
            "direct_source_match": False,
            "confidence": "low",
            "source_match_strength": str(context_quality.get("source_match_strength") or "weak"),
            "recommended_answer_mode": "clarify_then_answer",
            "reason": "The investment wrapper is ambiguous, so the lens should ask a clarifying question before a strong consultation.",
        }
    if clarifying_questions and context_quality.get("source_match_strength") in {"none", "weak"}:
        return {
            "direct_source_match": False,
            "confidence": "low",
            "source_match_strength": str(context_quality.get("source_match_strength") or "weak"),
            "recommended_answer_mode": "clarify_then_answer",
            "reason": "The question has instrument or evidence ambiguity and only weak direct lens support.",
        }
    if needs_company_data:
        return {
            "direct_source_match": bool(context_quality.get("direct_source_match")),
            "confidence": str(context_quality.get("confidence") or "medium"),
            "source_match_strength": str(context_quality.get("source_match_strength") or "related"),
            "recommended_answer_mode": "lens_with_company_bridge",
            "reason": "Guru lens context is available, but company-specific judgment needs filing evidence.",
        }
    return {
        "direct_source_match": bool(context_quality.get("direct_source_match")),
        "confidence": str(context_quality.get("confidence") or "medium"),
        "source_match_strength": str(context_quality.get("source_match_strength") or "related"),
        "recommended_answer_mode": "lens_grounded_answer",
        "reason": "Use the selected guru ontology lenses; label weak matches as lens application, not direct guru advice.",
    }


def _research_status_from_answerability(
    *,
    answerability: Mapping[str, Any],
    needs_company_data: bool,
    clarifying_questions: Sequence[str],
    selected_rows: Sequence[Mapping[str, Any]],
) -> str:
    if not selected_rows:
        return "ontology_gap"
    if needs_company_data:
        return "needs_company_evidence"
    if answerability.get("recommended_answer_mode") == "clarify_then_answer":
        return "needs_clarification"
    if answerability.get("direct_source_match"):
        return "sufficient_lens"
    if clarifying_questions and answerability.get("source_match_strength") in {"none", "weak"}:
        return "needs_clarification"
    return "partial_lens"


def _clarification_blocks_strong_answer(clarifying_questions: Sequence[str]) -> bool:
    return any("원자재 자체" in question for question in clarifying_questions)


def _clarifying_questions_for_question(
    question: str,
    needs_company_data: bool,
    ticker: str | None = None,
) -> list[str]:
    normalized = question.lower()
    questions: list[str] = []
    if _question_has_unresolved_asset_wrapper_ambiguity(question, ticker):
        questions.append("원자재 자체, ETF/선물, 생산 기업, 로열티/인프라 중 무엇에 투자한 건가요?")
    if needs_company_data and not _clean_optional(ticker) and not _has_ticker_like_token(question):
        if _question_has_named_company_phrase(question):
            questions.append(
                "공시 조회를 위해 정확한 티커와 거래소, 보통주/우선주 구분을 확인해 주세요."
            )
        else:
            questions.append("판단할 회사명이나 티커가 있나요?")
    if any(token in normalized for token in ("비중", "몰려", "포트폴리오", "평균단가", "손실")):
        questions.append("현재 비중, 매수 이유, 추가 매수/보유/매도 중 어떤 결정을 고민 중인가요?")
    return questions[:3]


def _question_has_asset_wrapper_ambiguity(question: str) -> bool:
    normalized = question.lower()
    if any(
        token in normalized
        for token in ("원유", "oil", "commodity", "원자재", "비트코인", "bitcoin")
    ):
        return True
    tokens = set(_tokens(normalized))
    return "금" in tokens or "gold" in tokens


def _question_has_unresolved_asset_wrapper_ambiguity(
    question: str,
    ticker: str | None,
) -> bool:
    return (
        _question_has_asset_wrapper_ambiguity(question)
        and not _clean_optional(ticker)
        and not _has_ticker_like_token(question)
    )


def _question_has_named_company_phrase(question: str) -> bool:
    normalized = re.sub(r"\s+", " ", question.lower()).strip()
    return any(
        token in normalized
        for token in (
            "라는 종목",
            "라는 회사",
            "라는 기업",
            "이란 종목",
            "이란 회사",
            "이란 기업",
            "called ",
            "named ",
        )
    )


def _persona_profile_from_rows(
    author_keys: Sequence[str],
    rows: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    source_rows = [
        row for row in rows if row.get("object_origin") in {"source_grounded", "corpus_synthesized"}
    ]
    caution_rows = [
        row
        for row in source_rows
        if row.get("object_type") in {"anti_pattern", "behavioral_warning", "risk_frame"}
    ]
    reasoning_rows = [
        row
        for row in source_rows
        if row.get("object_type")
        in {"principle", "decision_criterion", "heuristic", "valuation_frame", "time_horizon_frame"}
    ]
    return {
        "source": "guru_ontology",
        "selected_author_keys": list(author_keys),
        "derived_traits": _row_labels(source_rows[:5]),
        "reasoning_style": _row_labels(reasoning_rows[:5]),
        "caution_patterns": _row_labels(caution_rows[:5]),
        "policy": (
            "Persona is derived from selected ontology objects. Skills must not add hard-coded "
            "guru principles outside this pack."
        ),
    }


def _philosophy_context_from_lenses(
    *,
    selected_authors: Sequence[str],
    lens_rows: Sequence[Mapping[str, Any]],
    compact_lenses: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    """Expose source-grounded decision rules for a distinct virtual advisor.

    The main Guru agent consumes this compact context to formulate its own
    company-specific questions.  Every principle remains traceable to a
    selected reviewed object. It may shape a selected-author-inspired virtual
    advisor's reasoning and prose, but this function does not synthesize a
    personality, a conclusion, or an ungrounded causal claim.
    """
    compact_by_id = {
        str(item.get("reviewed_id")): item
        for item in compact_lenses
        if str(item.get("reviewed_id") or "").strip()
    }
    principles: list[dict[str, Any]] = []
    for row in lens_rows:
        reviewed_id = str(row.get("reviewed_id") or "").strip()
        compact = compact_by_id.get(reviewed_id)
        if not reviewed_id or compact is None:
            continue
        applicability = _public_soft_metadata(row.get("applicability"))
        source_anchors = _list_value(row.get("supporting_span_ids"))
        principle = {
            "principle_id": reviewed_id,
            "author_key": row.get("author_key"),
            "label": row.get("label_ko") or row.get("label_en"),
            "statement": row.get("summary_ko") or row.get("label_ko") or row.get("label_en"),
            "decision_role": _resolved_answer_role(row).get("default"),
            "evidence_to_seek": _list_value(compact.get("company_data_hooks")),
            "applies_when": _list_value(applicability.get("strong_for")),
            "weakened_when": _list_value(applicability.get("weak_for")),
            "rejection_conditions": _list_value(applicability.get("anti_triggers")),
            "clarify_when": _list_value(
                applicability.get("requires_clarification_when")
            ),
            "source_anchor_ids": [str(item) for item in source_anchors],
        }
        principles.append(
            {
                key: value
                for key, value in principle.items()
                if value not in (None, [], "")
            }
        )
    return {
        "format": "krw-guru-philosophy-context/v1",
        "selected_author_keys": list(selected_authors),
        "principles": principles,
        "boundary": (
            "Use only these source-grounded decision rules to select questions and "
            "shape a distinct selected-author-inspired virtual advisor. Do not claim "
            "to be or speak as the real person, invent a rule, or infer a company "
            "conclusion before verified filing evidence is returned."
        ),
    }


def _row_labels(rows: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    labels = []
    seen: set[str] = set()
    for row in rows:
        label = row.get("label_ko") or row.get("label_en")
        if not label or str(label) in seen:
            continue
        seen.add(str(label))
        labels.append(
            {
                "label_ko": row.get("label_ko"),
                "object_type": row.get("object_type"),
                "confidence": row.get("confidence"),
            }
        )
    return labels


def _source_anchors_for_rows(
    root_path: Path,
    rows: Sequence[Mapping[str, Any]],
    *,
    limit: int,
) -> list[dict[str, Any]]:
    span_index = _load_span_index_from_report(root_path)
    anchors: list[dict[str, Any]] = []
    seen: set[str] = set()
    for row in rows:
        for span_id in row.get("supporting_span_ids") or []:
            if span_id in seen:
                continue
            seen.add(str(span_id))
            metadata = span_index.get(str(span_id), {"span_id": span_id, "available": False})
            anchor = {
                "span_id": metadata.get("span_id"),
                "source_id": metadata.get("source_id"),
                "author_key": metadata.get("author_key") or row.get("author_key"),
                "title": metadata.get("title"),
                "official_url": metadata.get("official_url"),
                "section_title": metadata.get("section_title"),
                "available": metadata.get("available", False),
            }
            anchors.append({key: value for key, value in anchor.items() if value not in (None, "")})
            if len(anchors) >= limit:
                return anchors
    return anchors


def _trace_recommendations(
    question: str,
    lens_rows: Sequence[Mapping[str, Any]],
    consultation_rows: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    recommendations = []
    for row in [*lens_rows, *consultation_rows]:
        reviewed_id = row.get("reviewed_id")
        if not reviewed_id:
            continue
        recommendations.append(
            {
                "reviewed_id": reviewed_id,
                "label_ko": row.get("label_ko"),
                "reason": "Trace this object if the answer needs stronger source support.",
                "score": _score_row(question, row),
            }
        )
        if len(recommendations) >= 3:
            break
    return recommendations


def _guru_agent_autonomy(research_status: str, needs_company_data: bool) -> dict[str, Any]:
    if needs_company_data:
        return {
            "mode": "company_bridge_required",
            "allowed_next_tools": [
                "krw_guru_company_brief",
                "company_evidence_researcher Agent",
                "krw_guru_review_company_evidence",
            ],
            "optional_after_company_evidence": [
                "krw_guru_trace",
                "krw_guru_chain",
            ],
            "max_additional_tool_calls": 5,
            "stop_after": (
                "Do not answer a named-company judgment from Guru-only tools. "
                "First obtain company filing evidence through the app-provided "
                "company_evidence_researcher Agent, then review that evidence "
                "through krw_guru_review_company_evidence."
            ),
        }

    allowed = ["krw_guru_trace", "krw_guru_chain"]
    if research_status == "ontology_gap":
        allowed.append("krw_guru_search")
    return {
        "mode": "bounded",
        "allowed_next_tools": allowed,
        "max_additional_tool_calls": 2,
        "stop_after": (
            "Answer after query_context plus at most selected trace/chain calls; do not perform broad search loops."
        ),
    }


def _guru_do_not_call(
    research_status: str,
    *,
    needs_company_data: bool = False,
) -> list[str]:
    blocked = ["krw_guru_evidence as broad retrieval", "full raw JSONL reads"]
    if needs_company_data:
        blocked.extend(
            [
                "final company-specific answer before company_evidence_researcher",
                "krw_guru_trace or krw_guru_chain as a substitute for company filing evidence",
                "krw_guru_review_company_evidence with empty payload, retrieval plan, or company_filing_brief",
            ]
        )
    if research_status != "ontology_gap":
        blocked.append("krw_guru_search for broad re-ranking")
    return blocked


def _research_pack_warnings(
    context_quality: Mapping[str, Any],
    selected_rows: Sequence[Mapping[str, Any]],
) -> list[str]:
    warnings: list[str] = []
    if not selected_rows:
        warnings.append("No reviewed guru ontology objects selected.")
    if not context_quality.get("direct_source_match"):
        warnings.append("Direct source match is not strong; label the answer as lens application.")
    if int(context_quality.get("span_supported_rows") or 0) == 0:
        warnings.append("No source span anchors were available in the selected pack.")
    return warnings


def _evidence_grade_for_rows(rows: Sequence[Mapping[str, Any]]) -> str:
    if any(row.get("supporting_span_ids") for row in rows):
        return "direct"
    if any(row.get("object_origin") == "source_grounded" for row in rows):
        return "source_grounded_without_span_anchor"
    if rows:
        return "ontology_only"
    return "none"


def _resolve_root(root: str | Path | None) -> Path:
    if root is not None:
        return Path(root).expanduser().resolve()
    return guru_root(None)


def _load_point_reviewed_bundle(
    root_path: Path,
    *,
    reviewed_id: str,
    include_related: bool,
    include_neighbor_relationships: bool,
    related_limit: int | None,
) -> dict[str, Any]:
    """Point-load one object and its bounded adjacency from an admitted shard index."""
    author_keys = _author_keys_from_reviewed_id(reviewed_id)
    objects = load_guru_index_objects_by_id(
        root_path,
        reviewed_ids=[reviewed_id],
        author_keys=author_keys,
    )
    if objects is None:
        return _load_reviewed_bundle(root_path, author_keys=author_keys)

    relationships_by_id: dict[str, list[dict[str, Any]]] = {reviewed_id: []}
    related_ids: list[str] = []
    if include_related and reviewed_id in objects:
        selected_relationships = load_guru_index_relationships_by_id(
            root_path,
            reviewed_ids=[reviewed_id],
            author_keys=author_keys,
        )
        if selected_relationships is None:
            return _load_reviewed_bundle(root_path, author_keys=author_keys)
        relationships_by_id.update(selected_relationships)
        for relationship in selected_relationships.get(reviewed_id, []):
            other_id = (
                relationship.get("to_id")
                if relationship.get("from_id") == reviewed_id
                else relationship.get("from_id")
            )
            if other_id:
                related_ids.append(str(other_id))

    related_ids = _clean_list(related_ids)
    if related_limit is not None:
        related_ids = related_ids[: max(0, int(related_limit))]
    if related_ids:
        related_objects = load_guru_index_objects_by_id(
            root_path,
            reviewed_ids=related_ids,
            author_keys=author_keys,
        )
        if related_objects is None:
            return _load_reviewed_bundle(root_path, author_keys=author_keys)
        objects.update(related_objects)
        if include_neighbor_relationships:
            neighbor_relationships = load_guru_index_relationships_by_id(
                root_path,
                reviewed_ids=related_ids,
                author_keys=author_keys,
            )
            if neighbor_relationships is None:
                return _load_reviewed_bundle(root_path, author_keys=author_keys)
            relationships_by_id.update(neighbor_relationships)

    searchable: list[dict[str, Any]] = []
    files: dict[str, list[dict[str, Any]]] = {family: [] for family in REVIEWED_FILES}
    objects_by_id: dict[str, dict[str, Any]] = {}
    for object_id, source_row in objects.items():
        row = dict(source_row)
        object_family = _row_family(row)
        indexed_row = dict(row)
        if object_family:
            indexed_row["object_family"] = object_family
            family = FAMILY_BY_OBJECT_FAMILY.get(object_family)
            if family:
                files[family].append(row)
        searchable.append(indexed_row)
        objects_by_id[object_id] = indexed_row

    return {
        **files,
        "objects_by_id": objects_by_id,
        "relationships_by_id": relationships_by_id,
        "_index": {
            "enabled": True,
            "runtime_mode": "author_shard_point_lookup",
            "author_keys": author_keys or [],
            "payload_scope": "selected_and_neighbor_payloads",
            "payload_object_count": len(searchable),
            "relationships_scope": "selected_and_neighbor_relationships",
            "relationship_object_count": len(relationships_by_id),
        },
    }


def _load_reviewed_bundle(
    root_path: Path,
    *,
    author_keys: Sequence[str] | None = None,
    query: str | None = None,
    family_limits: Mapping[str, int] | None = None,
    object_types: Sequence[str] | None = None,
    intent_family: str | None = None,
    decision_stage: str | None = None,
    requires_company_data: bool | None = None,
    projection_profile: str = "context",
) -> dict[str, Any]:
    if family_limits is not None:
        candidate_bundle = _load_index_candidate_bundle(
            root_path,
            query=str(query or ""),
            author_keys=author_keys,
            family_limits=family_limits,
            object_types=object_types,
            intent_family=intent_family,
            decision_stage=decision_stage,
            requires_company_data=requires_company_data,
            projection_profile=projection_profile,
        )
        if candidate_bundle is not None:
            return candidate_bundle
        fallback_bundle = _load_reviewed_jsonl_bundle(root_path, author_keys=author_keys)
        fallback_bundle["_index"].update(
            {
                "candidate_generation": {
                    "mode": "reviewed_jsonl_fallback",
                    "candidate_exhaustive": True,
                    "fallback_reasons": ["candidate_index_unavailable"],
                },
                "payload_scope": "full_reviewed_jsonl_fallback",
                "relationships_scope": "full_reviewed_jsonl_fallback",
            }
        )
        return fallback_bundle
    try:
        indexed = load_guru_index_bundle(root_path, author_keys=author_keys)
    except (json.JSONDecodeError, OSError, sqlite3.DatabaseError, TypeError, ValueError):
        indexed = None
    if indexed is not None:
        return indexed
    return _load_reviewed_jsonl_bundle(root_path, author_keys=author_keys)


def _load_reviewed_jsonl_bundle(
    root_path: Path,
    *,
    author_keys: Sequence[str] | None,
) -> dict[str, Any]:
    reviewed_dir = root_path / "reviewed"
    files = {
        family: _read_jsonl(reviewed_dir / filename) for family, filename in REVIEWED_FILES.items()
    }
    searchable: list[dict[str, Any]] = []
    for family in ("guru_objects", "consultation_objects", "data_needs", "corpus_metadata"):
        for row in files[family]:
            row = dict(row)
            row["object_family"] = OBJECT_FAMILY_BY_FILE[family]
            searchable.append(row)
    objects_by_id = {str(row["reviewed_id"]): row for row in searchable if row.get("reviewed_id")}
    relationships_by_id: dict[str, list[dict[str, Any]]] = {}
    for relationship in files["relationships"]:
        if not isinstance(relationship, dict):
            continue
        for key in ("from_id", "to_id"):
            object_id = relationship.get(key)
            if object_id:
                relationships_by_id.setdefault(str(object_id), []).append(relationship)
    return {
        **files,
        "objects_by_id": objects_by_id,
        "relationships_by_id": relationships_by_id,
        "_index": {
            "enabled": False,
            "runtime_mode": "reviewed_jsonl",
            "author_keys": _clean_list(author_keys),
        },
    }


def _load_index_candidate_bundle(
    root_path: Path,
    *,
    query: str,
    author_keys: Sequence[str] | None,
    family_limits: Mapping[str, int],
    object_types: Sequence[str] | None,
    intent_family: str | None,
    decision_stage: str | None,
    requires_company_data: bool | None,
    projection_profile: str,
) -> dict[str, Any] | None:
    files: dict[str, list[dict[str, Any]]] = {family: [] for family in REVIEWED_FILES}
    objects_by_id: dict[str, dict[str, Any]] = {}
    family_diagnostics: dict[str, Any] = {}
    index_payload: Mapping[str, Any] | None = None
    for family in sorted(family_limits):
        object_family = OBJECT_FAMILY_BY_FILE.get(family)
        if not object_family:
            continue
        candidate_result = scan_guru_index_compact_rows(
            root_path,
            query=query,
            author_keys=author_keys,
            object_families=[object_family],
            object_types=object_types,
            intent_family=intent_family,
            decision_stage=decision_stage,
            requires_company_data=requires_company_data,
            fts_shadow_limit=max(1, int(family_limits[family])),
            projection_profile=projection_profile,
        )
        if candidate_result is None:
            return None
        rows = [dict(row) for row in candidate_result.get("rows") or [] if isinstance(row, Mapping)]
        files[family].extend(rows)
        for row in rows:
            reviewed_id = str(row.get("reviewed_id") or "")
            if reviewed_id:
                indexed_row = dict(row)
                indexed_row["object_family"] = object_family
                objects_by_id[reviewed_id] = indexed_row
        current_index = candidate_result.get("_index")
        if isinstance(current_index, Mapping):
            index_payload = current_index
            family_diagnostics[family] = dict(current_index.get("candidate_generation") or {})
    if index_payload is None:
        return None
    combined_diagnostics = _combined_candidate_diagnostics(family_diagnostics)
    return {
        **files,
        "objects_by_id": objects_by_id,
        "relationships_by_id": {},
        "_index": {
            **dict(index_payload),
            "candidate_generation": combined_diagnostics,
            "payload_scope": "exact_compact_projection",
            "relationships_scope": "not_loaded",
        },
    }


def _fts_shadow_limit(result_limit: int) -> int:
    return min(
        max(
            max(1, int(result_limit)) * GURU_FTS_SHADOW_MULTIPLIER,
            GURU_FTS_SHADOW_MIN,
        ),
        GURU_FTS_SHADOW_MAX,
    )


def _combined_candidate_diagnostics(
    family_diagnostics: Mapping[str, Mapping[str, Any]],
) -> dict[str, Any]:
    diagnostics = list(family_diagnostics.values())
    return {
        "mode": "exact_compact_scan",
        "candidate_count": sum(int(item.get("candidate_count") or 0) for item in diagnostics),
        "structural_total": sum(int(item.get("structural_total") or 0) for item in diagnostics),
        "candidate_exhaustive": all(bool(item.get("candidate_exhaustive")) for item in diagnostics),
        "exact_compact_row_count": sum(
            int(item.get("exact_compact_row_count") or 0) for item in diagnostics
        ),
        "payload_json_row_count": sum(
            int(item.get("payload_json_row_count") or 0) for item in diagnostics
        ),
        "compact_cache_hit_shards": sum(
            int(item.get("compact_cache_hit_shards") or 0) for item in diagnostics
        ),
        "compact_cache_all_hit": bool(diagnostics)
        and all(bool(item.get("compact_cache_all_hit")) for item in diagnostics),
        "fts_shadow_count": sum(int(item.get("fts_shadow_count") or 0) for item in diagnostics),
        "fallback_reasons": sorted(
            {
                str(reason)
                for item in diagnostics
                for reason in item.get("fallback_reasons") or []
                if reason
            }
        ),
        "families": {key: dict(value) for key, value in sorted(family_diagnostics.items())},
    }


def _is_candidate_bundle(bundle: Mapping[str, Any]) -> bool:
    index_payload = bundle.get("_index")
    return bool(
        isinstance(index_payload, Mapping)
        and index_payload.get("enabled")
        and isinstance(index_payload.get("candidate_generation"), Mapping)
    )


def _hydrate_candidate_payloads(
    root_path: Path,
    bundle: dict[str, Any],
    rows: Sequence[Mapping[str, Any]],
) -> None:
    """Replace selected compact projections with canonical full payloads in place."""
    if not _is_candidate_bundle(bundle):
        return
    mutable_rows = [row for row in rows if isinstance(row, dict)]
    reviewed_ids = _clean_list(row.get("reviewed_id") for row in mutable_rows)
    if not reviewed_ids:
        return
    index_payload = bundle.get("_index")
    author_keys = (
        list(index_payload.get("author_keys") or []) if isinstance(index_payload, Mapping) else None
    )
    payloads = load_guru_index_objects_by_id(
        root_path,
        reviewed_ids=reviewed_ids,
        author_keys=author_keys,
    )
    if payloads is None:
        payloads = _reviewed_objects_by_id(root_path, reviewed_ids)
        _append_candidate_fallback_reason(bundle, "payload_index_fallback_to_reviewed_jsonl")

    objects_by_id = bundle.setdefault("objects_by_id", {})
    hydrated = 0
    for row in mutable_rows:
        reviewed_id = str(row.get("reviewed_id") or "")
        payload = payloads.get(reviewed_id)
        if payload is None:
            continue
        exposed_object_family = row.get("object_family")
        object_family = str(row.get("_index_object_family") or _row_family(row) or "")
        row.clear()
        row.update(payload)
        if exposed_object_family:
            row["object_family"] = exposed_object_family
        indexed_payload = dict(payload)
        if object_family:
            indexed_payload["object_family"] = object_family
        objects_by_id[reviewed_id] = indexed_payload
        hydrated += 1
    if isinstance(index_payload, dict):
        hydrated_ids = index_payload.setdefault("_hydrated_payload_ids", set())
        if isinstance(hydrated_ids, set):
            hydrated_ids.update(reviewed_id for reviewed_id in payloads if reviewed_id)
        index_payload["payload_scope"] = "selected_payloads"
        index_payload["payload_object_count"] = (
            len(hydrated_ids) if isinstance(hydrated_ids, set) else hydrated
        )


def _hydrate_candidate_relationships(
    root_path: Path,
    bundle: dict[str, Any],
    rows: Sequence[Mapping[str, Any]],
) -> None:
    if not _is_candidate_bundle(bundle):
        return
    reviewed_ids = _clean_list(row.get("reviewed_id") for row in rows)
    if not reviewed_ids:
        return
    index_payload = bundle.get("_index")
    author_keys = (
        list(index_payload.get("author_keys") or []) if isinstance(index_payload, Mapping) else None
    )
    relationships = load_guru_index_relationships_by_id(
        root_path,
        reviewed_ids=reviewed_ids,
        author_keys=author_keys,
    )
    if relationships is None:
        relationships = _reviewed_relationships_by_id(root_path, reviewed_ids)
        _append_candidate_fallback_reason(bundle, "relationship_index_fallback_to_reviewed_jsonl")
    current = bundle.setdefault("relationships_by_id", {})
    for reviewed_id, items in relationships.items():
        current[reviewed_id] = list(items)
    if isinstance(index_payload, dict):
        hydrated_relationship_ids = index_payload.setdefault("_hydrated_relationship_ids", set())
        if isinstance(hydrated_relationship_ids, set):
            hydrated_relationship_ids.update(reviewed_ids)
        index_payload["relationships_scope"] = "selected_results"
        index_payload["relationship_object_count"] = (
            len(hydrated_relationship_ids)
            if isinstance(hydrated_relationship_ids, set)
            else len(reviewed_ids)
        )


def _hydrate_candidate_related_objects(
    root_path: Path,
    bundle: dict[str, Any],
    rows: Sequence[Mapping[str, Any]],
) -> None:
    if not _is_candidate_bundle(bundle):
        return
    _hydrate_candidate_relationships(root_path, bundle, rows)
    related_ids: list[str] = []
    for row in rows:
        related_ids.extend(_clean_list(row.get("related_reviewed_ids")))
        reviewed_id = str(row.get("reviewed_id") or "")
        for relationship in bundle.get("relationships_by_id", {}).get(reviewed_id, []):
            other_id = (
                relationship.get("to_id")
                if relationship.get("from_id") == reviewed_id
                else relationship.get("from_id")
            )
            if other_id:
                related_ids.append(str(other_id))
    missing_ids = [
        reviewed_id
        for reviewed_id in _clean_list(related_ids)
        if reviewed_id not in bundle.get("objects_by_id", {})
    ]
    if not missing_ids:
        return
    index_payload = bundle.get("_index")
    author_keys = (
        list(index_payload.get("author_keys") or []) if isinstance(index_payload, Mapping) else None
    )
    related_objects = load_guru_index_objects_by_id(
        root_path,
        reviewed_ids=missing_ids,
        author_keys=author_keys,
    )
    if related_objects is None:
        related_objects = _reviewed_objects_by_id(root_path, missing_ids)
        _append_candidate_fallback_reason(bundle, "object_index_fallback_to_reviewed_jsonl")
    objects_by_id = bundle.setdefault("objects_by_id", {})
    for reviewed_id, row in related_objects.items():
        object_family = _row_family(row)
        indexed_row = dict(row)
        if object_family:
            indexed_row["object_family"] = object_family
        objects_by_id[reviewed_id] = indexed_row
        family = FAMILY_BY_OBJECT_FAMILY.get(object_family)
        if family is not None and all(
            existing.get("reviewed_id") != reviewed_id for existing in bundle.setdefault(family, [])
        ):
            bundle[family].append(row)
    if isinstance(index_payload, dict):
        index_payload["related_payload_count"] = len(related_objects)


def _reviewed_relationships_by_id(
    root_path: Path,
    reviewed_ids: Sequence[str],
) -> dict[str, list[dict[str, Any]]]:
    requested = set(reviewed_ids)
    relationships_by_id = {reviewed_id: [] for reviewed_id in reviewed_ids}
    for relationship in _read_jsonl(root_path / "reviewed" / REVIEWED_FILES["relationships"]):
        for endpoint_key in ("from_id", "to_id"):
            endpoint = str(relationship.get(endpoint_key) or "")
            if endpoint in requested:
                relationships_by_id[endpoint].append(relationship)
    return relationships_by_id


def _reviewed_objects_by_id(
    root_path: Path,
    reviewed_ids: Sequence[str],
) -> dict[str, dict[str, Any]]:
    requested = set(reviewed_ids)
    objects_by_id: dict[str, dict[str, Any]] = {}
    for family, object_family in OBJECT_FAMILY_BY_FILE.items():
        for source_row in _read_jsonl(root_path / "reviewed" / REVIEWED_FILES[family]):
            reviewed_id = str(source_row.get("reviewed_id") or "")
            if reviewed_id not in requested:
                continue
            row = dict(source_row)
            row["object_family"] = object_family
            objects_by_id[reviewed_id] = row
    return objects_by_id


def _append_candidate_fallback_reason(bundle: dict[str, Any], reason: str) -> None:
    index_payload = bundle.get("_index")
    if not isinstance(index_payload, dict):
        return
    diagnostics = index_payload.get("candidate_generation")
    if not isinstance(diagnostics, dict):
        return
    reasons = list(diagnostics.get("fallback_reasons") or [])
    if reason not in reasons:
        reasons.append(reason)
    diagnostics["fallback_reasons"] = sorted(reasons)


def _bundle_runtime(bundle: Mapping[str, Any]) -> dict[str, Any]:
    index_payload = bundle.get("_index")
    if not isinstance(index_payload, Mapping):
        return {"mode": "reviewed_jsonl", "index_enabled": False}
    runtime = {
        "mode": index_payload.get("runtime_mode", "reviewed_jsonl"),
        "index_enabled": bool(index_payload.get("enabled")),
        "index_manifest_path": index_payload.get("manifest_path"),
        "author_keys": list(index_payload.get("author_keys") or []),
    }
    candidate_generation = index_payload.get("candidate_generation")
    if isinstance(candidate_generation, Mapping):
        runtime["candidate_generation"] = dict(candidate_generation)
        runtime["payload_scope"] = index_payload.get("payload_scope")
        runtime["relationships_scope"] = index_payload.get("relationships_scope")
        runtime["payload_object_count"] = int(index_payload.get("payload_object_count") or 0)
        runtime["relationship_object_count"] = int(
            index_payload.get("relationship_object_count") or 0
        )
    return runtime


def _author_keys_from_reviewed_id(reviewed_id: str) -> list[str] | None:
    parts = str(reviewed_id or "").split(":")
    if len(parts) >= 3 and parts[0] == "guru" and parts[1] in AUTHOR_DISPLAY_NAMES:
        return [parts[1]]
    return None


def _iter_searchable_rows(
    bundle: Mapping[str, Any],
    families: Sequence[str],
) -> Iterable[dict[str, Any]]:
    for family in families:
        for row in bundle.get(family, []):
            yield row


def _filter_rows(
    rows: Iterable[dict[str, Any]],
    *,
    author_keys: Sequence[str] | None = None,
    object_types: Sequence[str] | None = None,
    intent_family: str | None = None,
    decision_stage: str | None = None,
    requires_company_data: bool | None = None,
) -> list[dict[str, Any]]:
    authors = set(_clean_list(author_keys))
    types = set(_clean_list(object_types))
    intent = _clean_optional(intent_family)
    stage = _clean_optional(decision_stage)
    filtered = []
    for row in rows:
        if authors and row.get("author_key") not in authors:
            continue
        if types and row.get("object_type") not in types:
            continue
        if intent and row.get("intent_family") != intent:
            continue
        if stage and row.get("decision_stage") != stage:
            continue
        if (
            requires_company_data is not None
            and bool(row.get("requires_company_data")) != requires_company_data
        ):
            continue
        filtered.append(row)
    return filtered


def _rank_and_take(
    *,
    query: str,
    rows: Sequence[dict[str, Any]],
    limit: int,
) -> list[dict[str, Any]]:
    scored = [(max(_score_row(query, row), 0), row) for row in rows]
    scored.sort(
        key=lambda item: (
            -item[0],
            item[1].get("author_key") or "",
            item[1].get("reviewed_id") or "",
        )
    )
    return [row for _score, row in scored[:limit]]


def _score_row(query: str, row: Mapping[str, Any]) -> int:
    query_tokens = _meaningful_query_tokens(query)
    if not query_tokens:
        return 1
    haystack = _row_text(row)
    haystack_tokens = set(_tokens(haystack))
    score = 0
    for token in query_tokens:
        if token in haystack_tokens:
            score += 6
        elif (
            len(token) >= 3 or (_KOREAN_TOKEN_RE.fullmatch(token) is not None and len(token) >= 2)
        ) and token in haystack:
            score += 2
    author = str(row.get("author_key") or "")
    if author and author in _infer_author_keys(query):
        score += 6
    intent = _infer_intent_family(query)
    if intent and row.get("intent_family") == intent:
        score += 5
    object_type = str(row.get("object_type") or "")
    if object_type in {"answer_playbook", "question_template", "question_route"}:
        score += 1
    if _question_is_generic_lens_framework(query):
        haystack_lower = haystack.lower()
        query_lower = query.lower()
        for theme in ("ai", "데이터센터", "data center", "nvidia", "tesla"):
            if theme in haystack_lower and theme not in query_lower:
                score -= 3
    return score


def _context_score(
    query: str,
    row: Mapping[str, Any],
    *,
    intent_family: str | None,
    row_family: str,
) -> int:
    score = _score_row(query, row)
    score += _intent_object_type_bonus(intent_family, row, row_family=row_family)
    score += _soft_metadata_score(query, row, intent_family=intent_family, row_family=row_family)
    score += _data_need_intent_bonus(query, row, intent_family=intent_family, row_family=row_family)
    score += _semantic_relevance_bonus(query, row)
    score += _context_domain_adjustment(
        query, row, intent_family=intent_family, row_family=row_family
    )
    score -= _effective_theme_overfit_penalty(query, row, row_family=row_family)
    if intent_family and row.get("intent_family") == intent_family:
        score += 7
    if row_family == "guru_object":
        if row.get("object_origin") == "source_grounded":
            score += 3
        if row.get("supporting_span_ids"):
            score += 2
    return score


def _context_domain_adjustment(
    query: str,
    row: Mapping[str, Any],
    *,
    intent_family: str | None,
    row_family: str,
) -> int:
    if row_family not in {"guru_object", "data_need"}:
        return 0
    query_tags = _query_domain_tags(query)
    row_tags = _row_domain_tags(row)
    specificity = _mapping_value(row.get("specificity"))
    specificity_level = str(specificity.get("level") or "")
    score = 0

    if query_tags and row_tags:
        if query_tags & row_tags:
            score += 10 if row_family == "guru_object" else 6
        elif row_tags & sector_context_tags():
            score -= 10

    company_context = (
        bool(query_tags) or _has_ticker_like_token(query) or _question_mentions_company_need(query)
    )
    if company_context and row_family == "guru_object":
        if specificity_level == "general_principle":
            score += 4
        elif specificity_level == "company_case_specific" and not (query_tags & row_tags):
            score -= 10

    if _question_is_generic_lens_framework(query):
        if specificity_level == "general_principle":
            score += 4
        elif specificity_level in {
            "sector_specific",
            "asset_class_specific",
            "company_case_specific",
        }:
            score -= 8

    for tag in row_tags:
        score -= context_mismatch_penalty(
            tag,
            query_tags=query_tags,
            intent_family=intent_family,
        )
    for tag in query_tags & row_tags:
        score += context_match_bonus(tag)
    return _cap_unspecified_general_domain_penalty(
        score,
        row,
        row_family=row_family,
        specificity=specificity,
    )


def _cap_unspecified_general_domain_penalty(
    score: int,
    row: Mapping[str, Any],
    *,
    row_family: str,
    specificity: Mapping[str, Any],
) -> int:
    if (
        score < 0
        and row_family == "guru_object"
        and not specificity
        and str(row.get("object_type") or "") in {"principle", "decision_criterion"}
    ):
        return max(score, -UNSPECIFIED_GENERAL_DOMAIN_PENALTY_CAP)
    return score


def _intent_object_type_bonus(
    intent_family: str | None,
    row: Mapping[str, Any],
    *,
    row_family: str,
) -> int:
    object_type = str(row.get("object_type") or "")
    if row_family == "consultation_object":
        return CONSULTATION_OBJECT_TYPE_PRIORITIES.get(object_type, 0)
    if row_family != "guru_object":
        return 0
    priorities = CONTEXT_OBJECT_TYPE_PRIORITIES.get(intent_family or "")
    if not priorities:
        priorities = ("principle", "decision_criterion", "heuristic", "risk_frame", "anti_pattern")
    if object_type in priorities:
        return max(4, 16 - priorities.index(object_type) * 2)
    if intent_family in {"learn_guru_view", "business_quality_check"} and object_type in {
        "risk_frame",
        "behavioral_warning",
        "anti_pattern",
    }:
        return -8
    return 0


def _data_need_intent_bonus(
    query: str,
    row: Mapping[str, Any],
    *,
    intent_family: str | None,
    row_family: str,
) -> int:
    if row_family != "data_need":
        return 0
    text = _row_text(row)
    family = str(row.get("data_need_family") or "")
    score = 0
    if intent_family == "position_sizing":
        if family == "portfolio_context":
            score += 42
        if _contains_any(text, ("포트폴리오", "비중", "집중", "position", "concentration")):
            score += 12
        if _contains_any(text, ("리스크", "손실", "방어", "하락", "risk", "loss")):
            score += 8
    if intent_family in {"risk_check", "cyclical_risk_check"}:
        if family in {"future_company_section", "future_company_metric", "future_company_text"}:
            score += 6
        if _contains_any(
            text,
            ("risk_factors", "balance_sheet", "위험", "리스크", "부채", "유동성", "liquidity"),
        ):
            score += 12
    return score


def _soft_metadata_score(
    query: str,
    row: Mapping[str, Any],
    *,
    intent_family: str | None,
    row_family: str,
) -> int:
    score = 0
    applicability = _mapping_value(row.get("applicability"))
    specificity = _mapping_value(row.get("specificity"))
    answer_role = _mapping_value(row.get("answer_role"))

    if _metadata_values_match(query, intent_family, applicability.get("strong_for")):
        score += 14
    if _metadata_values_match(query, intent_family, applicability.get("possible_for")):
        score += 6
    if _metadata_values_match(query, intent_family, applicability.get("weak_for")):
        score -= 8
    if _metadata_values_match(query, intent_family, applicability.get("anti_triggers")):
        score -= 14
    if _metadata_values_match(
        query,
        intent_family,
        applicability.get("requires_clarification_when"),
    ):
        score += 5 if str(row.get("object_type") or "") == "clarifying_question" else -2

    specificity_level = str(specificity.get("level") or "")
    specificity_matches = _metadata_values_match(
        query,
        intent_family,
        [
            specificity.get("source_case_ko"),
            *_list_value(specificity.get("source_case_tags")),
        ],
    )
    if specificity_level == "general_principle":
        if _question_is_generic_lens_framework(query) or row_family == "guru_object":
            score += 2
    elif specificity_level in {
        "sector_specific",
        "asset_class_specific",
        "company_case_specific",
        "document_context_specific",
    }:
        if specificity_matches:
            score += 8
        elif _question_is_generic_lens_framework(query):
            score -= 7
        elif specificity_level == "company_case_specific":
            score -= 5
        else:
            score -= 3

    role_default = str(answer_role.get("default") or "")
    role_values = [role_default, *_list_value(answer_role.get("possible_roles"))]
    if row_family == "data_need" and "data_need" in role_values:
        score += 6
    if _question_is_generic_lens_framework(query) and "checklist" in role_values:
        score += 6
    if intent_family in {"risk_check", "cyclical_risk_check", "sell_or_trim"}:
        if any(role in role_values for role in ("caution", "contrast")):
            score += 5
    if intent_family in {
        "business_quality_check",
        "valuation_check",
        "capital_allocation_check",
        "thesis_review",
    }:
        if any(role in role_values for role in ("core_lens", "checklist")):
            score += 5
    if role_default == "core_lens":
        score += 3
    elif role_default == "supporting_lens":
        score += 1
    return score


def _metadata_values_match(
    query: str,
    intent_family: str | None,
    values: Any,
) -> bool:
    normalized_query = query.lower()
    query_tokens = {token for token in _meaningful_query_tokens(query) if len(token) >= 2}
    for raw_value in _list_value(values):
        value = str(raw_value).strip().lower()
        if not value:
            continue
        if intent_family and (
            value == intent_family
            or intent_family in value
            or intent_family.replace("_", " ") in value
        ):
            return True
        if value in normalized_query:
            return True
        value_tokens = {token for token in _tokens(value) if len(token) >= 2}
        if value_tokens and query_tokens and len(value_tokens & query_tokens) >= 1:
            return True
    return False


def _mapping_value(value: Any) -> Mapping[str, Any]:
    return value if isinstance(value, Mapping) else {}


def _list_value(value: Any) -> list[Any]:
    if value is None:
        return []
    if isinstance(value, list):
        return value
    if isinstance(value, tuple):
        return list(value)
    return [value]


def _semantic_relevance_bonus(query: str, row: Mapping[str, Any]) -> int:
    normalized_query = query.lower()
    haystack = _row_text(row)
    bonus = 0
    if _contains_any(normalized_query, ("훌륭한 사업", "좋은 사업", "좋은 회사", "사업 품질")):
        if _contains_any(
            haystack, ("경제성", "경쟁력", "좋은 기업", "좋은 사업", "quality", "현금 창출")
        ):
            bonus += 8
    if _contains_any(normalized_query, ("좋은 주식", "주식을 고르는", "고르는 체크리스트")):
        if _contains_any(
            haystack,
            (
                "기업 선택자",
                "좋은 기업",
                "지속 가능한",
                "경제적 특성",
                "품질",
                "진입장벽",
                "최저비용",
                "우수한 비즈니스",
                "내구 성장",
                "가격 결정력",
            ),
        ):
            bonus += 12
    if _contains_any(normalized_query, ("훌륭한 투자", "좋은 투자", "가격", "비싸", "안전마진")):
        if _contains_any(haystack, ("가격", "안전마진", "valuation", "비싸", "내재가치", "가치")):
            bonus += 7
    if _contains_any(normalized_query, ("하락장", "평균단가", "손실", "더 살지", "추가 매수")):
        if _contains_any(haystack, ("리스크", "손실", "방어", "사이클", "불확실", "매도", "하락")):
            bonus += 8
    if _contains_any(
        normalized_query,
        ("비중", "몰려", "집중", "포트폴리오", "현금 비중", "position", "sizing"),
    ):
        if _contains_any(
            haystack,
            ("포트폴리오", "비중", "집중", "분산", "리스크", "손실", "방어", "position"),
        ):
            bonus += 18
        if str(row.get("object_type") or "") in {
            "risk_frame",
            "behavioral_warning",
            "anti_pattern",
        }:
            bonus += 6
    if _contains_any(normalized_query, ("팔", "매도", "줄여", "축소", "trim", "sell")):
        if _contains_any(
            haystack,
            ("매도", "줄이", "축소", "thesis", "훼손", "리스크", "손실", "비중", "valuation"),
        ):
            bonus += 14
    if _contains_any(
        normalized_query, ("현금 비중", "현금 보유", "cash allocation", "cash position")
    ):
        if _contains_any(
            haystack, ("현금", "cash", "인내", "기회비용", "대기", "방어", "position")
        ):
            bonus += 10
    if _contains_any(normalized_query, ("가격 인상", "비용 구조", "자본배분", "activist", "개선")):
        if _contains_any(
            haystack, ("가격 인상", "비용", "마진", "자본배분", "개선", "촉매", "운영")
        ):
            bonus += 10
    if _contains_any(normalized_query, ("체크리스트", "질문 목록", "어떤 질문", "뭘 확인")):
        if str(row.get("object_type") or "") in {"question_template", "answer_playbook"}:
            bonus += 8
    return bonus


def _theme_overfit_penalty(query: str, row: Mapping[str, Any]) -> int:
    normalized_query = query.lower()
    haystack = _row_text(row)
    penalty = 0
    author_key = str(row.get("author_key") or "")
    for rule in overfit_penalty_rules():
        author_allowlist = {str(value) for value in _list_value(rule.get("author_allowlist"))}
        if author_allowlist and author_key in author_allowlist:
            continue
        row_terms = [str(value) for value in _list_value(rule.get("row_terms"))]
        query_terms = [str(value) for value in _list_value(rule.get("query_terms"))]
        row_matches = _contains_any(haystack, row_terms)
        query_matches = _contains_any(normalized_query, query_terms)
        if bool(rule.get("applies_when_query_matches")):
            if row_matches and query_matches:
                penalty += int(rule.get("penalty") or 0)
        elif row_matches and not query_matches:
            penalty += int(rule.get("penalty") or 0)
    return penalty


def _effective_theme_overfit_penalty(
    query: str,
    row: Mapping[str, Any],
    *,
    row_family: str,
) -> int:
    penalty = _theme_overfit_penalty(query, row)
    if row_family == "data_need":
        return min(penalty, DATA_NEED_THEME_PENALTY_CAP)
    return penalty


def _scoring_query(
    question: str,
    ticker: str | None,
    company_context: Mapping[str, Any] | None = None,
) -> str:
    clean_ticker = _clean_optional(ticker)
    parts = [question]
    if clean_ticker and not contains_context_term(question, clean_ticker):
        parts.append(clean_ticker)
    context_text = company_context_search_text(company_context)
    if context_text:
        parts.append(context_text)
    return " ".join(parts)


@lru_cache(maxsize=4096)
def _query_domain_tags(query: str) -> set[str]:
    return context_tags_for_text(query)


def _row_domain_tags(row: Mapping[str, Any]) -> set[str]:
    materialized = row.get("_ranking_domain_tags")
    if isinstance(materialized, list):
        return {str(value) for value in materialized}
    text = _row_text(row)
    specificity = _mapping_value(row.get("specificity"))
    source_tags = " ".join(
        str(value).lower() for value in _list_value(specificity.get("source_case_tags"))
    )
    haystack = f"{text} {source_tags}"
    return context_tags_for_text(haystack)


def _contains_any(value: str, terms: Sequence[str]) -> bool:
    return any(term in value for term in terms)


def _row_family(row: Mapping[str, Any]) -> str:
    family = row.get("object_family") or row.get("_index_object_family")
    if family:
        return str(family)
    if row.get("data_need_key") or row.get("data_need_family"):
        return "data_need"
    if row.get("object_origin") == "consultation_derived":
        return "consultation_object"
    if row.get("object_origin") in {"source_grounded", "corpus_synthesized"}:
        return "guru_object"
    return ""


def _row_text(row: Mapping[str, Any]) -> str:
    materialized = row.get("_ranking_text")
    if isinstance(materialized, str):
        return materialized
    return guru_ranking_text(row)


@lru_cache(maxsize=32768)
def _tokens(value: str) -> list[str]:
    return [token.lower() for token in _WORD_RE.findall(value)]


@lru_cache(maxsize=8192)
def _meaningful_query_tokens(value: str) -> tuple[str, ...]:
    meaningful: list[str] = []
    for raw_token in _tokens(value):
        if raw_token in _LOW_INFORMATION_QUERY_TOKENS:
            continue
        token = _normalize_query_token(raw_token)
        if token in _LOW_INFORMATION_QUERY_TOKENS:
            continue
        if _KOREAN_TOKEN_RE.fullmatch(token) and len(token) < 2:
            continue
        if token and token not in meaningful:
            meaningful.append(token)
    return tuple(meaningful)


def _normalize_query_token(token: str) -> str:
    normalized = token.lower().strip()
    if _KOREAN_TOKEN_RE.fullmatch(normalized):
        for suffix in _KOREAN_PREDICATE_SUFFIXES:
            if normalized.endswith(suffix) and len(normalized) - len(suffix) >= 2:
                normalized = normalized[: -len(suffix)]
                break
    for suffix in _KOREAN_PARTICLE_SUFFIXES:
        if normalized.endswith(suffix) and len(normalized) - len(suffix) >= 2:
            normalized = normalized[: -len(suffix)]
            break
    return normalized


def _compact_result(
    row: Mapping[str, Any],
    *,
    score: int,
    relationships: Mapping[str, list[dict[str, Any]]],
    include_relationships: bool = False,
    relationship_limit: int = 3,
) -> dict[str, Any]:
    reviewed_id = str(row.get("reviewed_id") or "")
    result = {
        "reviewed_id": reviewed_id,
        "object_family": row.get("object_family"),
        "author_key": row.get("author_key"),
        "author_name": AUTHOR_DISPLAY_NAMES.get(str(row.get("author_key")), row.get("author_key")),
        "object_type": row.get("object_type"),
        "object_origin": row.get("object_origin"),
        "label_ko": row.get("label_ko"),
        "label_en": row.get("label_en"),
        "summary_ko": row.get("summary_ko"),
        "intent_family": row.get("intent_family"),
        "decision_stage": row.get("decision_stage"),
        "requires_company_data": row.get("requires_company_data"),
        "requires_portfolio_data": row.get("requires_portfolio_data"),
        "requires_user_context": row.get("requires_user_context"),
        "data_need_family": row.get("data_need_family"),
        "data_need_key": row.get("data_need_key"),
        "company_data_hooks": row.get("company_data_hooks") or [],
        "supporting_span_ids": row.get("supporting_span_ids") or [],
        "applicability": _public_soft_metadata(row.get("applicability")),
        "specificity": _public_soft_metadata(row.get("specificity")),
        "answer_role": _resolved_answer_role(row),
        "confidence": row.get("confidence"),
        "score": score,
    }
    related = relationships.get(reviewed_id, [])
    if related:
        result["relationship_summary"] = _relationship_summary(related)
        if include_relationships:
            result["relationships"] = [
                {
                    "relationship_id": item.get("relationship_id"),
                    "relation_type": item.get("relation_type"),
                    "from_id": item.get("from_id"),
                    "to_id": item.get("to_id"),
                    "explanation_ko": item.get("explanation_ko"),
                }
                for item in related[:relationship_limit]
            ]
    return {key: value for key, value in result.items() if value not in (None, [], "", {})}


def _relationship_summary(relationships: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    by_type: Counter[str] = Counter()
    for relationship in relationships:
        relation_type = relationship.get("relation_type")
        if relation_type:
            by_type[str(relation_type)] += 1
    return {
        "count": len(relationships),
        "by_type": dict(sorted(by_type.items())),
    }


def _public_object(row: Mapping[str, Any]) -> dict[str, Any]:
    keys = (
        "reviewed_id",
        "object_family",
        "author_key",
        "object_type",
        "object_origin",
        "label_ko",
        "label_en",
        "summary_ko",
        "body_ko",
        "question_pattern_ko",
        "example_questions_ko",
        "intent_family",
        "intent_tags",
        "decision_stage",
        "answer_section_type",
        "answer_sections",
        "required_context",
        "requires_company_data",
        "requires_portfolio_data",
        "requires_user_context",
        "data_need_family",
        "data_need_key",
        "company_data_hooks",
        "supporting_span_ids",
        "applicability",
        "specificity",
        "answer_role",
        "confidence",
        "status",
    )
    return {key: row.get(key) for key in keys if row.get(key) not in (None, [], "", {})}


def _public_soft_metadata(value: Any) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        return {}
    return {str(key): nested for key, nested in value.items() if nested not in (None, [], "")}


def _resolved_answer_role(row: Mapping[str, Any]) -> dict[str, Any]:
    source_role = _public_soft_metadata(row.get("answer_role"))
    source_default = source_role.get("default")
    has_source_default = source_default is not None and (
        not isinstance(source_default, str) or bool(source_default.strip())
    )
    if has_source_default:
        return source_role

    row_family = _row_family(row)
    object_type = str(row.get("object_type") or "")
    if row_family == "data_need":
        default = "data_need"
    elif object_type in {"risk_frame", "behavioral_warning"}:
        default = "caution"
    elif object_type == "anti_pattern":
        default = "contrast"
    elif object_type in {"question_template", "answer_playbook", "question_route"}:
        default = "checklist"
    elif object_type in {"principle", "decision_criterion"}:
        default = "core_lens"
    else:
        default = "supporting_lens"
    resolved = dict(source_role)
    resolved["default"] = default
    resolved.setdefault("derived", True)
    resolved.setdefault(
        "provenance",
        "derived_from_object_family_and_object_type",
    )
    return resolved


def _read_json(path: Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return payload if isinstance(payload, dict) else {}


def _read_jsonl(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return rows
    for line in lines:
        if not line.strip():
            continue
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(payload, dict):
            rows.append(payload)
    return rows


def _count_jsonl(path: Path) -> int:
    try:
        with path.open(encoding="utf-8") as handle:
            return sum(1 for line in handle if line.strip())
    except OSError:
        return 0


def _load_span_index_from_report(root_path: Path) -> dict[str, dict[str, Any]]:
    report = _read_json(root_path / "reviewed" / "curation_report.json")
    running_root = os.environ.get(GURU_RUNNING_ROOT_ENV) or report.get("running_root")
    if not running_root:
        return {}
    manifest_path = Path(str(running_root)).expanduser() / "parsed_manifest.json"
    manifest = _read_json(manifest_path)
    span_index: dict[str, dict[str, Any]] = {}
    for document in manifest.get("parsed_documents") or []:
        if not isinstance(document, dict) or document.get("status") != "parsed":
            continue
        spans_path = document.get("spans_path")
        if not spans_path:
            continue
        for row in _read_jsonl(Path(str(spans_path)).expanduser()):
            span_id = row.get("span_id")
            if not span_id:
                continue
            span_index[str(span_id)] = {
                "span_id": span_id,
                "source_id": row.get("source_id"),
                "author_key": row.get("author_key"),
                "title": row.get("title"),
                "official_url": row.get("official_url"),
                "section_title": row.get("section_title"),
                "position": row.get("position"),
                "span_type": row.get("span_type"),
                "text_hash": row.get("text_hash"),
                "available": True,
                "text": row.get("text"),
            }
    return span_index


def _load_eval_questions() -> list[dict[str, Any]]:
    path = _eval_questions_path()
    rows = _read_jsonl(path)
    if rows:
        return rows
    return []


def _eval_questions_path() -> Path:
    raw_path = os.environ.get(GURU_EVAL_QUESTIONS_PATH_ENV)
    if raw_path:
        return Path(raw_path).expanduser()
    repo_root = Path(__file__).resolve().parents[3]
    plugin_level_path = (
        repo_root / "plugins" / "krw-guru-advisor" / "references" / "eval-questions.jsonl"
    )
    if plugin_level_path.is_file():
        return plugin_level_path
    return (
        repo_root
        / "plugins"
        / "krw-guru-advisor"
        / "skills"
        / "krw-guru-advisor"
        / "references"
        / "eval-questions.jsonl"
    )


def _eval_coverage(rows: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    lens_counts: Counter[str] = Counter()
    intent_counts: Counter[str] = Counter()
    company_required = 0
    for row in rows:
        if row.get("requires_company_evidence"):
            company_required += 1
        intent = row.get("intent")
        if intent:
            intent_counts[str(intent)] += 1
        for lens in row.get("lenses") or []:
            lens_counts[str(lens)] += 1
    return {
        "lenses": dict(sorted(lens_counts.items())),
        "intents": dict(sorted(intent_counts.items())),
        "requires_company_evidence": company_required,
        "guru_only": len(rows) - company_required,
    }


@lru_cache(maxsize=4096)
def _infer_author_keys(question: str) -> list[str]:
    normalized = question.lower()
    if any(token in normalized for token in ("구루들", "여러 구루", "멀티 구루", "multi-guru")):
        return ["buffett", "terry_smith", "marks", "ackman", "flatt"]
    aliases = {
        "buffett": ("buffett", "버핏", "워런", "warren"),
        "marks": ("marks", "막스", "하워드", "howard", "oaktree"),
        "ackman": ("ackman", "애크먼", "어크만", "bill ackman", "pershing"),
        "flatt": ("flatt", "플랫", "브루스", "brookfield"),
        "terry_smith": ("terry smith", "terry_smith", "테리", "스미스", "fundsmith"),
    }
    matched = [
        author_key
        for author_key, values in aliases.items()
        if any(value in normalized for value in values)
    ]
    return matched


def _default_authors_for_question(question: str) -> list[str]:
    normalized = question.lower()
    if any(token in normalized for token in ("리스크", "손실", "하락", "사이클", "불확실")):
        return ["marks", "buffett"]
    if any(token in normalized for token in ("activist", "개선", "턴어라운드", "자본환원")):
        return ["ackman", "buffett"]
    if any(token in normalized for token in ("인프라", "실물", "자산운용", "asset")):
        return ["flatt", "buffett"]
    if any(token in normalized for token in ("품질", "quality", "장기복리", "compound")):
        return ["terry_smith", "buffett"]
    return ["buffett", "marks"]


@lru_cache(maxsize=4096)
def _infer_intent_family(question: str) -> str | None:
    families = _infer_intent_families(question)
    return families[0] if families else None


@lru_cache(maxsize=4096)
def _infer_intent_families(question: str) -> list[str]:
    normalized = question.lower()
    if (
        _contains_any(normalized, ("좋은 주식", "주식을 고르는"))
        and "체크리스트" in normalized
        and not _contains_any(
            normalized,
            ("비중", "몰려", "집중", "포트폴리오", "리스크", "위험", "손실", "activist", "개선"),
        )
    ):
        return ["learn_guru_view"]
    if _question_denies_company_subject(question) and _contains_any(
        normalized,
        ("질문부터", "질문 목록", "어떤 질문"),
    ):
        if _contains_any(normalized, ("비중", "몰려", "집중", "포트폴리오", "position", "sizing")):
            return ["position_sizing"]
        if _contains_any(normalized, ("리스크", "위험", "손실", "하락", "불편", "downside")):
            return ["risk_check"]
        if _contains_any(
            normalized, ("activist", "개선", "턴어라운드", "turnaround", "비용 구조", "가격 인상")
        ):
            return ["thesis_review"]
        if _contains_any(normalized, ("비싸", "밸류", "valuation", "per", "가격", "안전마진")):
            return ["valuation_check"]
        return ["learn_guru_view"]
    patterns = (
        ("considering_buy", ("매수 전", "사기 전", "buy before", "before buying")),
        ("cyclical_risk_check", ("사이클", "cycle", "하락장")),
        ("position_sizing", ("비중", "몰려", "집중", "포트폴리오", "position", "sizing")),
        ("holding_review", ("샀", "보유", "들고", "투자했", "holding", "장기 보유")),
        ("risk_check", ("리스크", "위험", "손실", "하락", "불편", "downside")),
        (
            "thesis_review",
            ("activist", "개선", "턴어라운드", "turnaround", "비용 구조", "가격 인상"),
        ),
        ("capital_allocation_check", ("자본배분", "자사주", "배당", "capital allocation")),
        (
            "valuation_check",
            ("비싸", "비싼", "고평가", "밸류", "valuation", "per", "가격", "안전마진"),
        ),
        (
            "business_quality_check",
            (
                "좋은 회사",
                "좋은 사업",
                "좋은 주식",
                "훌륭한 사업",
                "훌륭한 투자",
                "사업 구조",
                "사업 품질",
                "quality",
                "품질",
                "현금흐름",
                "성장성",
                "compounder",
                "장기복리",
                "장기 복리",
                "인프라",
                "실물자산",
                "자산운용",
            ),
        ),
        ("contrarian_check", ("좋은 뉴스", "과열", "컨센서스", "반대로")),
        ("sell_or_trim", ("팔", "매도", "trim", "sell")),
        (
            "learn_guru_view",
            ("원칙", "철학", "설명", "배우", "체크리스트", "질문부터", "질문 목록"),
        ),
    )
    matched: list[str] = []
    for intent, tokens in patterns:
        if any(token in normalized for token in tokens):
            matched.append(intent)
    if _contains_any(
        normalized,
        ("너무 올랐", "많이 올랐", "급등", "고평가", "비싸", "비싼", "살까", "사도"),
    ):
        matched.extend(["valuation_check", "risk_check", "considering_buy"])
    if _contains_any(normalized, ("줄여", "축소", "팔아야", "매도", "trim", "sell")):
        matched.extend(["sell_or_trim", "position_sizing", "risk_check"])
    if _contains_any(normalized, ("현금 비중", "현금 보유", "cash allocation", "cash position")):
        matched.extend(["position_sizing", "risk_check"])
    if _contains_any(normalized, ("추가 매수", "더 살지", "물타기", "평균단가")):
        matched.extend(["position_sizing", "risk_check", "holding_review"])
    return list(dict.fromkeys(matched))


def _question_mentions_company_need(question: str) -> bool:
    if (
        _question_denies_company_subject(question)
        or _question_is_generic_lens_framework(question)
        or _question_mentions_non_company_instrument(question)
    ):
        return False
    normalized = question.lower()
    if _has_ticker_like_token(question):
        return True
    return any(
        token in normalized
        for token in (
            "회사",
            "종목",
            "기업",
            "샀",
            "보유",
            "매수",
            "매도",
            "공시",
            "10-k",
            "10q",
            "10-q",
            "재무",
            "현금흐름",
            "부채",
            "마진",
        )
    )


def _question_denies_company_subject(question: str) -> bool:
    normalized = re.sub(r"\s+", " ", question.lower()).strip()
    negative_patterns = (
        "종목은 아직 없어",
        "종목은 없어",
        "종목 없어",
        "종목 없음",
        "특정 종목 없음",
        "특정 종목은 없어",
        "아직 종목은 없어",
        "아직 종목 없어",
        "티커 없음",
        "티커는 없어",
        "아직 티커는 없어",
        "아직 티커 없어",
        "특정 티커는 없어",
        "특정 티커 없어",
        "ticker 없음",
        "no ticker",
        "no specific stock",
        "without a ticker",
        "아직 회사는 없어",
        "회사는 아직 없어",
        "특정 회사 없음",
        "특정 회사는 없어",
        "회사명 없이",
        "회사명은 없어",
        "회사명 없어",
        "회사 없이",
    )
    return any(pattern in normalized for pattern in negative_patterns)


@lru_cache(maxsize=4096)
def _question_is_generic_lens_framework(question: str) -> bool:
    """Return true for no-company guru framework questions.

    These questions may mention "company" or "stock" as an abstract object,
    but they are asking for a thinking frame rather than facts about a named
    company.
    """
    if _has_ticker_like_token(question):
        return False
    normalized = re.sub(r"\s+", " ", question.lower()).strip()
    framework_patterns = (
        "체크리스트",
        "질문 목록",
        "어떤 질문",
        "질문부터",
        "무엇을 확인",
        "뭘 확인",
        "어떻게 점검",
        "어떻게 생각",
        "어떻게 봐야",
        "어떤 데이터",
        "무슨 데이터",
        "필요한 데이터",
        "데이터가 필요",
        "보려면",
        "어떤 순서",
        "순서로 확인",
        "차이를 설명",
        "차이",
        "다르게",
        "비교",
        "설명해",
        "고를 때",
        "볼 때",
        "종목 없이",
        "종목은 빼",
        "종목을 빼",
        "종목은 아직",
        "특정 종목",
        "종목은 없어",
        "회사를 고를",
        "좋은 회사를",
        "좋은 사업",
        "좋은 뉴스",
        "성장성",
        "현금흐름",
        "주식을 고르는",
        "매수 전 질문",
        "포트폴리오",
        "평균단가",
        "현금 비중",
        "현금 보유",
        "cash position",
        "cash allocation",
        "손실 중",
        "안전마진",
        "사이클 리스크",
        "하락장",
    )
    return any(pattern in normalized for pattern in framework_patterns)


def _generic_filing_requirements(question: str, intent_family: str | None = None) -> list[str]:
    normalized = question.lower()
    inferred_intent = intent_family or _infer_intent_family(question)
    requirements: list[str] = []
    if inferred_intent == "risk_check" or any(
        token in normalized for token in ("리스크", "위험", "손실", "하락", "downside", "막스")
    ):
        requirements.extend(
            [
                "risk_factors",
                "balance_sheet",
                "cash_flow",
                "demand_cycle_exposure",
                "margin_pressure",
                "liquidity",
            ]
        )
    elif inferred_intent == "valuation_check":
        requirements.extend(
            [
                "valuation_context",
                "cash_generation",
                "margin_structure",
                "growth_duration",
                "risk_factors",
            ]
        )
    elif inferred_intent == "capital_allocation_check":
        requirements.extend(
            [
                "capital_allocation",
                "cash_flow",
                "share_repurchases",
                "dividends",
                "reinvestment",
                "balance_sheet",
            ]
        )
    elif inferred_intent in {"holding_review", "business_quality_check"}:
        requirements.extend(
            [
                "business_model",
                "cash_flow",
                "capital_allocation",
                "balance_sheet",
                "risk_factors",
                "valuation_context",
            ]
        )
    else:
        requirements.extend(
            [
                "business_model",
                "cash_flow",
                "capital_allocation",
                "balance_sheet",
                "risk_factors",
            ]
        )
    return list(dict.fromkeys(requirements))


def _filing_requirements_with_company_context(
    question: str,
    intent_family: str | None,
    company_context: Mapping[str, Any] | None,
) -> list[str]:
    requirements = _generic_filing_requirements(question, intent_family)
    requirements.extend(company_context_topics(company_context))
    return list(dict.fromkeys(value for value in requirements if value))


def _has_ticker_like_token(question: str, *, ticker: str | None = None) -> bool:
    if ticker:
        pattern = rf"(?<![A-Za-z0-9]){re.escape(ticker)}(?=[^A-Za-z0-9]|$)"
        return re.search(pattern, question) is not None
    matches = re.findall(r"(?<![A-Za-z0-9])([A-Z]{1,5})(?=[^A-Za-z0-9]|$)", question)
    return any(token not in NON_TICKER_ACRONYMS for token in matches)


def _question_mentions_non_company_instrument(question: str) -> bool:
    normalized = question.lower()
    return any(
        token in normalized
        for token in (
            " etf",
            "etf",
            "index fund",
            "인덱스 펀드",
            "인덱스펀드",
            "펀드",
            "선물",
            "futures",
            "option",
            "옵션",
        )
    )


def _filing_bridge_brief(question: str, rows: Sequence[Mapping[str, Any]]) -> str:
    hooks = _generic_filing_requirements(question)
    for row in rows:
        hooks.extend(_normalized_company_hooks(row.get("company_data_hooks") or []))
        data_key = row.get("data_need_key")
        if data_key:
            hooks.append(str(data_key))
    hook_text = ", ".join(dict.fromkeys(hooks[:12]))
    if hook_text:
        return (
            "For the investor question, retrieve filing evidence for: "
            f"{hook_text}. User question: {question}"
        )
    return (
        "For the investor question, retrieve filing evidence for business model, "
        "cash flow, capital allocation, balance sheet, valuation context, and risk factors. "
        f"User question: {question}"
    )


def _normalized_company_hooks(values: Any) -> list[str]:
    if isinstance(values, str):
        return [values.strip()] if values.strip() else []
    if not isinstance(values, Iterable):
        return []
    hooks: list[str] = []
    for value in values:
        if isinstance(value, Mapping):
            for key in ("key", "metric", "section", "topic", "field", "table", "data_key"):
                nested = value.get(key)
                if isinstance(nested, str) and nested.strip():
                    hooks.append(nested.strip())
                    break
            continue
        if isinstance(value, str) and value.strip():
            hooks.append(value.strip())
    return list(dict.fromkeys(hooks))


def _filing_bridge_payload(
    question: str,
    rows: Sequence[Mapping[str, Any]],
    *,
    intent_family: str | None,
    company_context: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    requirements = _filing_requirements_with_company_context(
        question,
        intent_family,
        company_context,
    )
    company_context_payload = (
        company_context.model_dump(mode="json", exclude_none=True)
        if hasattr(company_context, "model_dump")
        else dict(company_context or {})
    )
    requires_company = _question_mentions_company_need(question)
    if not requires_company:
        return {
            "use_existing_krw_ontology_mcp": False,
            "filing_evidence_requirements": [],
            "conditional_filing_evidence_requirements": requirements,
            "company_context": company_context_payload,
            "next_step": "Select a company or ticker before calling KRW Ontology filing research.",
            "brief_en": (
                "No company or ticker is selected. Treat these as conditional filing checks "
                "for later company research, not as a request for company facts."
            ),
            "boundary": "This tool identifies guru-derived evidence needs only; it does not answer company facts.",
        }
    return {
        "use_existing_krw_ontology_mcp": True,
        "filing_evidence_requirements": requirements,
        "conditional_filing_evidence_requirements": [],
        "company_context": company_context_payload,
        "brief_en": _filing_bridge_brief(question, rows),
        "boundary": "This tool identifies evidence needs only; it does not answer company facts.",
    }


def _clean_list(values: Sequence[str] | None) -> list[str]:
    if not values:
        return []
    return [value.strip() for value in values if isinstance(value, str) and value.strip()]


def _clean_optional(value: str | None) -> str | None:
    if value is None:
        return None
    stripped = value.strip()
    return stripped or None


def _limit(value: int, *, maximum: int) -> int:
    if isinstance(value, bool):
        return 10
    return max(1, min(int(value), maximum))


def _limited_words(value: str, *, max_words: int) -> str:
    words = value.split()
    return " ".join(words[:max_words])


def _json_arg(value: Any, *, field_name: str) -> dict[str, Any]:
    if not value:
        return {}
    if isinstance(value, Mapping):
        return dict(value)
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError as exc:
        raise ValueError(f"{field_name} must be valid JSON") from exc
    if not isinstance(parsed, Mapping):
        raise ValueError(f"{field_name} must decode to a JSON object")
    return dict(parsed)


def _company_brief_next_step(brief: Mapping[str, Any]) -> str:
    if brief.get("requires_identifier_clarification") or brief.get("missing_inputs"):
        return "Resolve company identifier before calling KRW Ontology company filing research."
    if brief.get("requires_company_evidence"):
        return "Pass company_research_question_ko or company_research_question_en to KRW Ontology company filing research."
    return "No company evidence is required yet; keep the answer guru-only unless a company is selected."


def _json(payload: Mapping[str, Any]) -> str:
    return json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True)


def _status_markdown(payload: Mapping[str, Any]) -> str:
    counts = payload.get("counts") if isinstance(payload.get("counts"), Mapping) else {}
    return "\n".join(
        [
            "# KRW Guru Advisor MCP Status",
            "",
            f"- ok: {payload.get('ok')}",
            f"- root: `{payload.get('root')}`",
            f"- guru objects: {counts.get('guru_objects', 0)}",
            f"- consultation objects: {counts.get('consultation_objects', 0)}",
            f"- data needs: {counts.get('data_needs', 0)}",
            f"- relationships: {counts.get('relationships', 0)}",
        ]
    )


def _search_markdown(payload: Mapping[str, Any]) -> str:
    lines = [f"# Guru Search Results: {payload.get('query')}", ""]
    for result in payload.get("results") or []:
        lines.append(f"- **{result.get('label_ko')}** ({result.get('author_name')})")
        lines.append(f"  - {result.get('summary_ko')}")
    return "\n".join(lines)


def _query_context_markdown(payload: Mapping[str, Any]) -> str:
    answerability = (
        payload.get("answerability") if isinstance(payload.get("answerability"), Mapping) else {}
    )
    research_pack = (
        payload.get("research_pack") if isinstance(payload.get("research_pack"), Mapping) else {}
    )
    lines = [
        "# KRW Guru Query Context",
        "",
        f"- research_status: {payload.get('research_status')}",
        f"- direct_source_match: {answerability.get('direct_source_match')}",
        f"- confidence: {answerability.get('confidence')}",
        f"- recommended_answer_mode: {answerability.get('recommended_answer_mode')}",
        f"- requires_company_evidence: {payload.get('requires_company_evidence')}",
        f"- allowed_next_tools: {', '.join((payload.get('agent_autonomy') or {}).get('allowed_next_tools') or [])}",
    ]
    clarifying = research_pack.get("clarifying_questions") or []
    if clarifying:
        lines.append("- clarifying_questions:")
        for question in clarifying:
            lines.append(f"  - {question}")
    lines.append("- selected_lenses:")
    for result in research_pack.get("selected_lenses") or []:
        lines.append(f"  - {result.get('label_ko')} ({result.get('author_name')})")
    return "\n".join(lines)


def _company_brief_markdown(payload: Mapping[str, Any]) -> str:
    brief = (
        payload.get("company_filing_brief")
        if isinstance(payload.get("company_filing_brief"), Mapping)
        else {}
    )
    lines = [
        "# Guru Company Filing Brief",
        "",
        f"- research_status: {payload.get('research_status')}",
        f"- requires_company_evidence: {payload.get('requires_company_evidence')}",
        f"- subject: {((brief.get('company_identity') or {}) if isinstance(brief.get('company_identity'), Mapping) else {}).get('subject')}",
        f"- next_step: {payload.get('next_step')}",
        "",
        "## Company Research Question",
        "",
        str(brief.get("company_research_question_ko") or ""),
    ]
    topics = brief.get("required_filing_topics") or []
    if topics:
        lines.extend(["", "## Required Filing Topics"])
        lines.extend(f"- {topic}" for topic in topics[:12])
    return "\n".join(lines)


def _company_pack_markdown(payload: Mapping[str, Any]) -> str:
    company_pack = (
        payload.get("company_pack") if isinstance(payload.get("company_pack"), Mapping) else {}
    )
    render_plan = (
        payload.get("render_plan") if isinstance(payload.get("render_plan"), Mapping) else {}
    )
    identity = (
        company_pack.get("company_identity")
        if isinstance(company_pack.get("company_identity"), Mapping)
        else {}
    )
    lines = [
        "# Guru Company Research Pack",
        "",
        f"- research_status: {payload.get('research_status')}",
        f"- author_key: {company_pack.get('author_key')}",
        f"- subject: {identity.get('subject')}",
        f"- opening_style: {render_plan.get('opening_style')}",
        f"- first_question: {render_plan.get('first_question')}",
    ]
    missing = company_pack.get("missing_evidence") or []
    if missing:
        lines.extend(
            [
                "",
                "## Internal Follow-Up Checks",
                "Use these silently as concise next checks; do not print a data-limitation section.",
            ]
        )
        lines.extend(f"- {item}" for item in missing[:12])
    return "\n".join(lines)


def _company_evidence_review_markdown(payload: Mapping[str, Any]) -> str:
    review = (
        payload.get("company_evidence_review")
        if isinstance(payload.get("company_evidence_review"), Mapping)
        else {}
    )
    identity = (
        review.get("company_identity")
        if isinstance(review.get("company_identity"), Mapping)
        else {}
    )
    lines = [
        "# Guru Company Evidence Review",
        "",
        f"- research_status: {payload.get('research_status')}",
        f"- author_key: {review.get('author_key')}",
        f"- subject: {identity.get('subject')}",
        f"- lens_alignment: {review.get('lens_alignment')}",
        "",
        "## Internal Interpretation",
        "",
        str(review.get("primary_interpretation_ko") or ""),
    ]
    emphasize = review.get("what_to_emphasize") or []
    if emphasize:
        lines.extend(["", "## Emphasize"])
        lines.extend(f"- {item}" for item in emphasize[:8])
    avoid = review.get("what_not_to_overstate") or []
    if avoid:
        lines.extend(["", "## Do Not Overstate"])
        lines.extend(f"- {item}" for item in avoid[:8])
    return "\n".join(lines)


def _context_markdown(payload: Mapping[str, Any]) -> str:
    lines = ["# Guru Consultation Context", "", f"질문: {payload.get('question')}", ""]
    lines.append(f"- company evidence required: {payload.get('requires_company_evidence')}")
    lines.append("- selected lenses:")
    for result in payload.get("lens_objects") or []:
        lines.append(f"  - {result.get('label_ko')} ({result.get('author_name')})")
    lines.append("- data needs:")
    for result in payload.get("data_needs") or []:
        lines.append(f"  - {result.get('label_ko')}")
    return "\n".join(lines)


def _evidence_markdown(payload: Mapping[str, Any]) -> str:
    obj = payload.get("object") if isinstance(payload.get("object"), Mapping) else {}
    lines = [f"# {obj.get('label_ko')}", "", str(obj.get("summary_ko") or "")]
    spans = payload.get("supporting_spans") or []
    if spans:
        lines.extend(["", "## Supporting Spans"])
        for span in spans:
            lines.append(
                f"- {span.get('title') or span.get('span_id')} ({span.get('official_url')})"
            )
    return "\n".join(lines)


def _chain_markdown(payload: Mapping[str, Any]) -> str:
    obj = payload.get("object") if isinstance(payload.get("object"), Mapping) else {}
    chain = payload.get("chain") if isinstance(payload.get("chain"), Mapping) else {}
    lines = [f"# {obj.get('label_ko')}", "", "- semantic_neighbors:"]
    for neighbor in chain.get("semantic_neighbors") or []:
        lines.append(f"  - {neighbor.get('label_ko')} ({neighbor.get('object_type')})")
    return "\n".join(lines)


def _data_needs_markdown(payload: Mapping[str, Any]) -> str:
    lines = ["# Guru Data Needs", "", f"질문: {payload.get('question')}", ""]
    for result in payload.get("data_needs") or []:
        lines.append(f"- **{result.get('label_ko')}**: {result.get('summary_ko')}")
    return "\n".join(lines)


def _eval_markdown(payload: Mapping[str, Any]) -> str:
    lines = ["# Guru Advisor Eval Questions", ""]
    for row in payload.get("questions") or []:
        lines.append(f"- `{row.get('id')}` {row.get('question')}")
    return "\n".join(lines)
