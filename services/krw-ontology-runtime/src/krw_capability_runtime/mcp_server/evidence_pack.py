"""Deterministic source-lineage verification for company evidence handoffs.

The Guru Advisor subagent may choose candidate ontology objects, but it must not
author the provenance contract itself. This module resolves each exact object
id through the serving index and emits a bounded, hash-stable evidence pack.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
import hashlib
import json
import re
from typing import Any

from krw_capability_runtime.agent_index.store import (
    OntologyStore,
    filing_document_roles_from_documents,
)


VERIFIED_COMPANY_EVIDENCE_FORMAT = "krw-verified-company-evidence/v1"
MAX_VERIFICATION_QUESTIONS = 5
MAX_VERIFICATION_OBJECTS = 8
MAX_OBJECTS_PER_QUESTION = 4
MAX_VERIFIED_EXCERPT_CHARS = 520
_QUESTION_ID_PATTERN = re.compile(r"^[A-Za-z0-9_.:-]{1,80}$")
_BRIEF_HASH_PATTERN = re.compile(r"^[a-f0-9]{64}$")
_FILING_DOCUMENT_TYPES = {"10-K", "10-Q"}
_DIRECT_OBJECT_TYPES = {"ResearchClaim", "EvidenceQuote", "SourceSpan"}
_METRIC_OBJECT_TYPES = {"MetricObservation", "XBRLFact", "Calculation"}


class EvidencePackInputError(ValueError):
    """A correctable public MCP input error for evidence-pack construction."""

    def __init__(
        self,
        *,
        code: str,
        message: str,
        required_change: str,
        invalid_fields: list[str],
    ) -> None:
        super().__init__(message)
        self.code = code
        self.required_change = required_change
        self.invalid_fields = invalid_fields


def build_verified_company_evidence_pack(
    *,
    store: OntologyStore,
    ticker: str,
    questions: Sequence[Mapping[str, Any]],
    release_id: str | None = None,
    brief_hash: str | None = None,
) -> dict[str, Any]:
    """Resolve exact object ids into a bounded, source-backed evidence pack."""

    normalized_ticker = str(ticker or "").strip().upper()
    if not normalized_ticker:
        raise ValueError("ticker is required")
    normalized_questions = _normalize_questions(questions)
    normalized_brief_hash = _normalize_brief_hash(brief_hash)
    documents = store.list_documents(
        ticker=normalized_ticker,
        document_types=sorted(_FILING_DOCUMENT_TYPES),
    )
    roles = filing_document_roles_from_documents(
        documents,
        tickers=[normalized_ticker],
    ).get(normalized_ticker, {})
    current_driver = roles.get("current_driver") if isinstance(roles, Mapping) else None
    annual_baseline = roles.get("annual_baseline") if isinstance(roles, Mapping) else None

    trace_cache: dict[str, dict[str, Any] | None] = {}
    evidence_by_question: list[dict[str, Any]] = []
    rejected_refs: list[dict[str, Any]] = []
    verified_object_ids: set[str] = set()

    for question in normalized_questions:
        question_evidence: list[dict[str, Any]] = []
        question_id = question["question_id"]
        for object_id in question["object_ids"]:
            if object_id not in trace_cache:
                trace_cache[object_id] = store.trace(object_id)
            trace = trace_cache[object_id]
            verified, rejection_reason = _verified_evidence_item(
                trace=trace,
                requested_object_id=object_id,
                ticker=normalized_ticker,
                current_driver=current_driver,
                annual_baseline=annual_baseline,
            )
            if verified is None:
                rejected_refs.append(
                    {
                        "question_id": question_id,
                        "object_id": object_id,
                        "reason": rejection_reason or "unverified",
                    }
                )
                continue
            question_evidence.append(verified)
            verified_object_ids.add(object_id)
        evidence_by_question.append(
            {
                "question_id": question_id,
                "evidence": question_evidence,
                "verified_count": len(question_evidence),
                "answerability": (
                    "verified"
                    if any(item["usable_for_strong_claim"] for item in question_evidence)
                    else "interpretation_only"
                    if question_evidence
                    else "not_verified"
                ),
            }
        )

    strong_claim_count = sum(
        1
        for question in evidence_by_question
        for item in question["evidence"]
        if item["usable_for_strong_claim"]
    )
    interpretation_count = sum(
        1
        for question in evidence_by_question
        for item in question["evidence"]
        if item["usable_for_interpretation"]
    )
    payload: dict[str, Any] = {
        "format": VERIFIED_COMPANY_EVIDENCE_FORMAT,
        "release_id": release_id,
        "ticker": normalized_ticker,
        **({"brief_hash": normalized_brief_hash} if normalized_brief_hash else {}),
        "current_driver": current_driver,
        "annual_baseline": annual_baseline,
        "evidence_by_question": evidence_by_question,
        "rejected_refs": rejected_refs,
        "verification_summary": {
            "question_count": len(evidence_by_question),
            "requested_object_count": len(trace_cache),
            "verified_object_count": len(verified_object_ids),
            "strong_claim_evidence_count": strong_claim_count,
            "interpretation_evidence_count": interpretation_count,
            "all_questions_have_verified_evidence": bool(evidence_by_question)
            and all(question["verified_count"] > 0 for question in evidence_by_question),
        },
        "usage_policy": {
            "strong_company_claims": (
                "Use only entries where usable_for_strong_claim is true."
            ),
            "interpretation": (
                "Entries marked interpretation-only may frame a question but must not be "
                "presented as direct filing proof."
            ),
            "current_period": (
                "Use current_driver for current conditions and annual_baseline for annual "
                "business structure or historical risk context."
            ),
        },
    }
    payload["pack_hash"] = evidence_pack_hash(payload)
    return payload


def evidence_pack_hash(payload: Mapping[str, Any]) -> str:
    """Return a stable hash after removing the self-referential hash field."""

    canonical = {key: value for key, value in payload.items() if key != "pack_hash"}
    encoded = json.dumps(
        canonical,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def is_verified_company_evidence_pack(payload: Mapping[str, Any]) -> bool:
    """Validate the deterministic pack envelope and its canonical hash."""

    if payload.get("format") != VERIFIED_COMPANY_EVIDENCE_FORMAT:
        return False
    supplied_hash = payload.get("pack_hash")
    if not isinstance(supplied_hash, str) or len(supplied_hash) != 64:
        return False
    return supplied_hash == evidence_pack_hash(payload)


def _normalize_questions(
    questions: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    if not questions:
        raise ValueError("questions must contain at least one item")
    if len(questions) > MAX_VERIFICATION_QUESTIONS:
        raise ValueError(
            f"questions supports at most {MAX_VERIFICATION_QUESTIONS} items"
        )

    normalized: list[dict[str, Any]] = []
    all_object_ids: set[str] = set()
    seen_question_ids: set[str] = set()
    for raw in questions:
        question_id = str(raw.get("question_id") or "").strip()
        if not _QUESTION_ID_PATTERN.fullmatch(question_id):
            raise ValueError(
                "question_id must be 1-80 characters using letters, numbers, '.', ':', '_' or '-'"
            )
        if question_id in seen_question_ids:
            raise ValueError(f"duplicate question_id: {question_id}")
        seen_question_ids.add(question_id)

        raw_object_ids = raw.get("object_ids")
        if not isinstance(raw_object_ids, Sequence) or isinstance(
            raw_object_ids, (str, bytes)
        ):
            raise ValueError(f"object_ids must be an array for {question_id}")
        object_ids: list[str] = []
        for raw_object_id in raw_object_ids:
            object_id = str(raw_object_id or "").strip()
            if object_id and object_id not in object_ids:
                object_ids.append(object_id)
        if len(object_ids) > MAX_OBJECTS_PER_QUESTION:
            raise EvidencePackInputError(
                code="too_many_object_ids_for_question",
                message=(
                    f"Question '{question_id}' supports at most "
                    f"{MAX_OBJECTS_PER_QUESTION} object ids; received {len(object_ids)}."
                ),
                required_change=(
                    f"Keep only the {MAX_OBJECTS_PER_QUESTION} most decision-relevant "
                    "object ids for this question and omit lower-priority evidence."
                ),
                invalid_fields=[f"questions[{len(normalized)}].object_ids"],
            )
        all_object_ids.update(object_ids)
        normalized.append({"question_id": question_id, "object_ids": object_ids})

    if len(all_object_ids) > MAX_VERIFICATION_OBJECTS:
        raise EvidencePackInputError(
            code="too_many_unique_object_ids",
            message=(
                f"The verification request has {len(all_object_ids)} unique object ids; "
                f"the maximum is {MAX_VERIFICATION_OBJECTS}."
            ),
            required_change=(
                f"Keep no more than {MAX_VERIFICATION_OBJECTS} unique object ids across "
                "all questions, prioritizing the direct evidence for the main tension."
            ),
            invalid_fields=["questions"],
        )
    return normalized


def _normalize_brief_hash(value: str | None) -> str | None:
    if value is None:
        return None
    normalized = str(value).strip().lower()
    if not _BRIEF_HASH_PATTERN.fullmatch(normalized):
        raise ValueError("brief_hash must be a lowercase SHA-256 hex digest")
    return normalized


def _verified_evidence_item(
    *,
    trace: Mapping[str, Any] | None,
    requested_object_id: str,
    ticker: str,
    current_driver: Mapping[str, Any] | None,
    annual_baseline: Mapping[str, Any] | None,
) -> tuple[dict[str, Any] | None, str | None]:
    if not trace:
        return None, "object_not_found"
    obj = trace.get("object") if isinstance(trace.get("object"), Mapping) else {}
    document = (
        trace.get("document") if isinstance(trace.get("document"), Mapping) else {}
    )
    evidence = (
        trace.get("evidence") if isinstance(trace.get("evidence"), Mapping) else {}
    )
    object_ticker = str(obj.get("ticker") or document.get("ticker") or "").upper()
    if object_ticker != ticker:
        return None, "ticker_mismatch"
    if str(obj.get("review_status") or "").lower() == "rejected":
        return None, "rejected_object"

    document_type = _normalize_document_type(
        obj.get("document_type")
        or document.get("document_type")
        or document.get("doc_type_key")
    )
    if document_type not in _FILING_DOCUMENT_TYPES:
        return None, "unsupported_document_type"
    period = str(obj.get("period") or document.get("period") or "").strip() or None
    if not period:
        return None, "missing_period"

    object_type = str(obj.get("type") or "")
    claims = _mapping_list(evidence.get("claims"))
    quotes = _mapping_list(evidence.get("quotes"))
    spans = _mapping_list(evidence.get("spans"))
    metric_lineage = (
        evidence.get("metric_lineage")
        if isinstance(evidence.get("metric_lineage"), Mapping)
        else None
    )
    has_metric_lineage = bool(
        metric_lineage
        and (
            metric_lineage.get("xbrl_facts")
            or metric_lineage.get("source_document_ids")
            or metric_lineage.get("calculation")
        )
    )
    has_direct_text_lineage = bool(quotes and spans)
    is_source_span = object_type == "SourceSpan" and bool(obj.get("text"))
    is_xbrl_fact = object_type == "XBRLFact" and bool(obj.get("source_document_id"))
    has_interpretation_lineage = bool(claims and quotes and spans)

    if object_type in _METRIC_OBJECT_TYPES and (has_metric_lineage or is_xbrl_fact):
        evidence_grade = "metric_lineage"
        trace_status = "traceable_metric_lineage"
        usable_for_strong_claim = True
    elif object_type in _DIRECT_OBJECT_TYPES and (has_direct_text_lineage or is_source_span):
        evidence_grade = "direct"
        trace_status = "traceable_direct"
        usable_for_strong_claim = True
    elif has_interpretation_lineage:
        evidence_grade = "supported_related"
        trace_status = "traceable_related"
        usable_for_strong_claim = False
    else:
        return None, "missing_source_lineage"

    anchor_roles = _anchor_roles(
        period=period,
        document_type=document_type,
        current_driver=current_driver,
        annual_baseline=annual_baseline,
    )
    section = _first_nonempty(
        obj.get("section_name"),
        obj.get("section_key"),
        *(quote.get("section_name") for quote in quotes),
        *(span.get("section_name") for span in spans),
    )
    verified_excerpt = _verified_excerpt(
        obj=obj,
        claims=claims,
        quotes=quotes,
        spans=spans,
        metric_lineage=metric_lineage,
    )
    return (
        {
            "source_object_id": requested_object_id,
            "object_type": object_type,
            "trace_status": trace_status,
            "evidence_grade": evidence_grade,
            "usable_for_strong_claim": usable_for_strong_claim,
            "usable_for_interpretation": True,
            "document": {
                "ticker": ticker,
                "document_type": document_type,
                "period": period,
                "section": section,
                "source_document_id": obj.get("source_document_id"),
                "anchor_roles": anchor_roles,
            },
            "claim_ids": [str(item["id"]) for item in claims if item.get("id")],
            "quote_ids": [str(item["id"]) for item in quotes if item.get("id")],
            "span_ids": [str(item["id"]) for item in spans if item.get("id")],
            "verified_excerpt": verified_excerpt,
            "metric_lineage": _compact_metric_lineage(metric_lineage),
        },
        None,
    )


def _normalize_document_type(value: Any) -> str:
    text = str(value or "").strip().upper().replace("_", "-")
    if text == "10K":
        return "10-K"
    if text == "10Q":
        return "10-Q"
    return text


def _anchor_roles(
    *,
    period: str,
    document_type: str,
    current_driver: Mapping[str, Any] | None,
    annual_baseline: Mapping[str, Any] | None,
) -> list[str]:
    roles: list[str] = []
    for role, anchor in (
        ("current_driver", current_driver),
        ("annual_baseline", annual_baseline),
    ):
        if not anchor:
            continue
        if str(anchor.get("period") or "") != period:
            continue
        if _normalize_document_type(anchor.get("document_type")) != document_type:
            continue
        roles.append(role)
    return roles or ["historical_context"]


def _verified_excerpt(
    *,
    obj: Mapping[str, Any],
    claims: Sequence[Mapping[str, Any]],
    quotes: Sequence[Mapping[str, Any]],
    spans: Sequence[Mapping[str, Any]],
    metric_lineage: Mapping[str, Any] | None,
) -> str | None:
    candidates: list[Any] = []
    if metric_lineage:
        candidates.append(metric_lineage.get("formatted_value"))
    candidates.extend(
        [
            obj.get("claim_text"),
            obj.get("quote_text"),
            *(claim.get("claim_text") for claim in claims),
        ]
    )
    candidates.extend(quote.get("quote_text") for quote in quotes)
    candidates.extend(
        [
            obj.get("description"),
            obj.get("name"),
            obj.get("text"),
        ]
    )
    candidates.extend(span.get("text") for span in spans)
    excerpt = _first_nonempty(*candidates)
    if not excerpt:
        return None
    compact = " ".join(str(excerpt).split())
    return compact[:MAX_VERIFIED_EXCERPT_CHARS]


def _compact_metric_lineage(
    lineage: Mapping[str, Any] | None,
) -> dict[str, Any] | None:
    if not lineage:
        return None
    calculation = lineage.get("calculation")
    return {
        "trace_type": lineage.get("trace_type"),
        "formatted_value": lineage.get("formatted_value"),
        "calculation_id": (
            calculation.get("id") if isinstance(calculation, Mapping) else None
        ),
        "input_metric_ids": [
            str(item["id"])
            for item in _mapping_list(lineage.get("input_metrics"))
            if item.get("id")
        ],
        "xbrl_fact_ids": [
            str(item["id"])
            for item in _mapping_list(lineage.get("xbrl_facts"))
            if item.get("id")
        ],
        "source_document_ids": [
            str(value)
            for value in lineage.get("source_document_ids") or []
            if value
        ],
    }


def _mapping_list(value: Any) -> list[Mapping[str, Any]]:
    if not isinstance(value, Sequence) or isinstance(value, (str, bytes)):
        return []
    return [item for item in value if isinstance(item, Mapping)]


def _first_nonempty(*values: Any) -> str | None:
    for value in values:
        if value is None:
            continue
        text = str(value).strip()
        if text:
            return text
    return None
