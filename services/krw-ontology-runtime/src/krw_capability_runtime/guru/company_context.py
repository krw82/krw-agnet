"""Runtime company context helpers for guru advisor orchestration."""

from __future__ import annotations

from collections.abc import Mapping, Sequence
import json
from typing import Any

from krw_capability_runtime.guru.context_taxonomy import context_tags_for_text
from krw_capability_runtime.guru.models import GuruCompanyOntologyContext


CONTEXT_VALUE_KEYS = {
    "activity",
    "activities",
    "available_company_topics",
    "business",
    "business_context_terms",
    "business_model",
    "business_model_terms",
    "company_topics",
    "context_terms",
    "context_tags",
    "driver",
    "drivers",
    "exposure",
    "exposures",
    "exposure_terms",
    "industry",
    "risk",
    "risk_context_terms",
    "risk_terms",
    "sector",
    "segment",
    "segments",
    "tag",
    "tags",
    "term",
    "terms",
    "topic",
    "topics",
}
MAX_CONTEXT_TERMS = 80
MAX_TOPIC_TERMS = 40


def parse_company_context_json(value: str | None) -> dict[str, Any] | None:
    if value is None or not value.strip():
        return None
    parsed = json.loads(value)
    if not isinstance(parsed, Mapping):
        raise ValueError("company_context_json must decode to an object")
    return dict(parsed)


def coerce_company_context(
    value: Mapping[str, Any] | GuruCompanyOntologyContext | None,
    *,
    ticker: str | None = None,
    company_name: str | None = None,
) -> GuruCompanyOntologyContext | None:
    if value is None:
        return None
    if isinstance(value, GuruCompanyOntologyContext):
        return value
    payload = dict(value)
    existing_format = payload.get("format")
    if existing_format == "krw-guru-company-ontology-context/v1":
        return GuruCompanyOntologyContext.model_validate(
            {
                **payload,
                "ticker": payload.get("ticker") or ticker,
                "company_name": payload.get("company_name") or company_name,
            }
        )

    business_terms = _unique_strings(
        [
            *_direct_strings(payload, "business_context_terms"),
            *_direct_strings(payload, "business_model_terms"),
            *_direct_strings(payload, "business_model"),
            *_direct_strings(payload, "segments"),
            *_direct_strings(payload, "activities"),
        ],
        limit=MAX_CONTEXT_TERMS,
    )
    risk_terms = _unique_strings(
        [
            *_direct_strings(payload, "risk_context_terms"),
            *_direct_strings(payload, "risk_terms"),
            *_direct_strings(payload, "risk_factors"),
            *_direct_strings(payload, "exposures"),
            *_direct_strings(payload, "exposure_terms"),
        ],
        limit=MAX_CONTEXT_TERMS,
    )
    available_topics = _unique_strings(
        [
            *_direct_strings(payload, "available_company_topics"),
            *_direct_strings(payload, "company_topics"),
            *_direct_strings(payload, "topics"),
            *_direct_strings(payload, "topic_map"),
        ],
        limit=MAX_TOPIC_TERMS,
    )
    context_terms = _unique_strings(
        [
            *_direct_strings(payload, "context_terms"),
            *_direct_strings(payload, "context_tags"),
            *_direct_strings(payload, "sector"),
            *_direct_strings(payload, "industry"),
            *_walk_context_strings(payload),
            *business_terms,
            *risk_terms,
            *available_topics,
        ],
        limit=MAX_CONTEXT_TERMS,
    )
    tag_text = " ".join([*context_terms, *business_terms, *risk_terms, *available_topics])
    context_tags = _unique_strings(
        [
            *_direct_strings(payload, "context_tags"),
            *sorted(context_tags_for_text(tag_text)),
        ],
        limit=MAX_TOPIC_TERMS,
    )
    return GuruCompanyOntologyContext(
        ticker=_string_or_none(payload.get("ticker")) or ticker,
        company_name=_string_or_none(payload.get("company_name")) or company_name,
        source=_string_or_none(payload.get("source")) or "company_ontology_runtime",
        context_terms=context_terms,
        business_context_terms=business_terms,
        risk_context_terms=risk_terms,
        available_company_topics=available_topics,
        context_tags=context_tags,
        confidence=_confidence_or_none(payload.get("confidence")),
        source_payload_keys=sorted(str(key) for key in payload.keys())[:MAX_TOPIC_TERMS],
    )


def company_context_search_text(context: GuruCompanyOntologyContext | Mapping[str, Any] | None) -> str:
    model = context if isinstance(context, GuruCompanyOntologyContext) else coerce_company_context(context)
    if model is None:
        return ""
    return " ".join(
        [
            *(model.context_terms or []),
            *(model.business_context_terms or []),
            *(model.risk_context_terms or []),
            *(model.available_company_topics or []),
            *(model.context_tags or []),
        ]
    )


def company_context_topics(context: GuruCompanyOntologyContext | Mapping[str, Any] | None) -> list[str]:
    model = context if isinstance(context, GuruCompanyOntologyContext) else coerce_company_context(context)
    if model is None:
        return []
    if model.available_company_topics:
        return _unique_strings(model.available_company_topics, limit=MAX_TOPIC_TERMS)
    return _unique_strings(model.context_terms, limit=MAX_TOPIC_TERMS)


def _walk_context_strings(value: Any, *, key: str = "", depth: int = 0) -> list[str]:
    if depth > 4:
        return []
    if isinstance(value, Mapping):
        results: list[str] = []
        for child_key, child_value in value.items():
            child_key_text = str(child_key)
            if _is_context_key(child_key_text):
                results.extend(_strings_from_value(child_value))
            results.extend(_walk_context_strings(child_value, key=child_key_text, depth=depth + 1))
        return results
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
        results = []
        for item in list(value)[:100]:
            results.extend(_walk_context_strings(item, key=key, depth=depth + 1))
        return results
    return []


def _direct_strings(payload: Mapping[str, Any], key: str) -> list[str]:
    if key not in payload:
        return []
    return _strings_from_value(payload.get(key))


def _strings_from_value(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        clean = value.strip()
        return [clean] if clean else []
    if isinstance(value, Mapping):
        results: list[str] = []
        for nested_key in ("name", "label", "title", "key", "topic", "term", "text", "summary"):
            nested = value.get(nested_key)
            if isinstance(nested, str) and nested.strip():
                results.append(nested.strip())
        return results
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
        results = []
        for item in value:
            results.extend(_strings_from_value(item))
        return results
    return [str(value)]


def _is_context_key(key: str) -> bool:
    normalized = key.lower()
    return normalized in CONTEXT_VALUE_KEYS or any(
        token in normalized
        for token in (
            "activity",
            "business",
            "driver",
            "exposure",
            "industry",
            "risk",
            "sector",
            "segment",
            "tag",
            "term",
            "topic",
        )
    )


def _unique_strings(values: Sequence[Any], *, limit: int) -> list[str]:
    results: list[str] = []
    seen: set[str] = set()
    for value in values:
        text = str(value).strip()
        if not text:
            continue
        normalized = text.lower()
        if normalized in seen:
            continue
        seen.add(normalized)
        results.append(text)
        if len(results) >= limit:
            break
    return results


def _string_or_none(value: Any) -> str | None:
    if isinstance(value, str) and value.strip():
        return value.strip()
    return None


def _confidence_or_none(value: Any) -> str | None:
    text = _string_or_none(value)
    return text if text in {"low", "medium", "high"} else None
