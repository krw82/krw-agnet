"""Data-driven context taxonomy for guru lens ranking and filing bridge filters."""

from __future__ import annotations

from functools import lru_cache
import json
import re
from pathlib import Path
from typing import Any, Mapping, Sequence


_TAXONOMY_PATH = Path(__file__).with_name("context_taxonomy.json")


@lru_cache(maxsize=1)
def load_context_taxonomy() -> dict[str, Any]:
    return json.loads(_TAXONOMY_PATH.read_text(encoding="utf-8"))


def context_tags_for_text(value: str) -> set[str]:
    tags: set[str] = set()
    for tag, config in _domains().items():
        terms = _string_list(config.get("terms"))
        if any(contains_context_term(value, term) for term in terms):
            tags.add(tag)
    return tags


def sector_context_tags() -> set[str]:
    return {
        tag
        for tag, config in _domains().items()
        if bool(config.get("sector_specific"))
    }


def context_match_bonus(tag: str) -> int:
    config = _domains().get(tag, {})
    return int(config.get("match_bonus") or 0)


def context_mismatch_penalty(
    tag: str,
    *,
    query_tags: set[str],
    intent_family: str | None,
) -> int:
    if tag in query_tags:
        return 0
    config = _domains().get(tag, {})
    allowed_intents = set(_string_list(config.get("intent_allowlist")))
    if intent_family and intent_family in allowed_intents:
        return 0
    return int(config.get("mismatch_penalty") or 0)


def filing_topic_filter_reason(topic: str, *, context_tags: set[str]) -> str | None:
    normalized = _normalize_topic(topic)
    for tag, config in _domains().items():
        if tag in context_tags:
            continue
        if _matches_topic_patterns(normalized, config.get("topic_patterns")):
            return str(config.get("filter_reason") or f"question does not mention {tag} context")
    if _matches_topic_patterns(normalized, load_context_taxonomy().get("non_filing_topic_patterns")):
        return "topic is a behavioral or market-sentiment lens, not a direct company filing requirement"
    return None


def overfit_penalty_rules() -> list[Mapping[str, Any]]:
    rules = load_context_taxonomy().get("overfit_penalty_rules")
    if isinstance(rules, list):
        return [rule for rule in rules if isinstance(rule, Mapping)]
    return []


def contains_context_term(value: str, term: str) -> bool:
    value_lower = value.lower()
    term_lower = str(term).lower().strip()
    if not term_lower:
        return False
    if re.fullmatch(r"[a-z0-9.]{1,8}", term_lower):
        pattern = rf"(?<![a-z0-9]){re.escape(term_lower)}(?![a-z0-9])"
        return re.search(pattern, value_lower) is not None
    return term_lower in value_lower


def _domains() -> Mapping[str, Mapping[str, Any]]:
    domains = load_context_taxonomy().get("domains")
    if isinstance(domains, Mapping):
        return {
            str(key): value
            for key, value in domains.items()
            if isinstance(value, Mapping)
        }
    return {}


def _matches_topic_patterns(normalized_topic: str, patterns: Any) -> bool:
    compact = normalized_topic.replace("_", "")
    return any(
        str(pattern).replace("_", "").replace(" ", "").lower() in compact
        for pattern in _string_list(patterns)
    )


def _normalize_topic(topic: str) -> str:
    return str(topic).lower().replace("-", "_").replace(" ", "_")


def _string_list(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        return [value]
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
        return [str(item) for item in value if item not in (None, "")]
    return [str(value)]
