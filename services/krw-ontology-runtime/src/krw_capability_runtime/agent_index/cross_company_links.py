"""Cross-company chain link generation for the v3 global spine."""

from __future__ import annotations

import hashlib
import json
import math
import re
import sqlite3
from collections import defaultdict
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from typing import Any

CROSS_COMPANY_LINK_BUILDER_VERSION = "cross-company-links/v1"

_GENERIC_TOKENS = {
    "and",
    "are",
    "business",
    "company",
    "companies",
    "cost",
    "costs",
    "demand",
    "effect",
    "factor",
    "financial",
    "growth",
    "impact",
    "increase",
    "market",
    "may",
    "operation",
    "operations",
    "performance",
    "price",
    "prices",
    "product",
    "products",
    "revenue",
    "revenues",
    "risk",
    "risks",
    "sales",
    "service",
    "services",
    "supply",
    "the",
}


@dataclass(frozen=True)
class CrossCompanyLinkGenerationResult:
    inserted: int
    exact_links: int
    similarity_links: int
    key_count: int
    skipped_generic_keys: int


def generate_cross_company_links(
    conn: sqlite3.Connection,
    *,
    replace: bool = True,
    max_tickers_per_key: int = 80,
    max_links_per_key: int = 300,
    max_links_per_ticker: int = 500,
    max_links_per_pair: int = 8,
    max_refs_per_similarity_token: int = 200,
    min_similarity_score: float = 0.35,
) -> CrossCompanyLinkGenerationResult:
    """Populate ``global_chain_index`` from global spine refs.

    The generator creates deterministic sparse links. Exact normalized keys are
    the primary signal; topic token overlap adds cross-company candidates when
    exact keys differ but topic language is materially similar.
    """
    conn.row_factory = sqlite3.Row
    if replace:
        conn.execute("DELETE FROM global_chain_index")
    refresh_global_key_stats(conn)
    key_stats = _key_stats(conn)
    exact_rows, skipped_generic_keys = _exact_key_links(
        conn,
        key_stats=key_stats,
        max_tickers_per_key=max_tickers_per_key,
        max_links_per_key=max_links_per_key,
    )
    similarity_rows = _topic_similarity_links(
        conn,
        max_refs_per_similarity_token=max_refs_per_similarity_token,
        min_similarity_score=min_similarity_score,
    )
    all_rows = _sparsify_links(
        [*exact_rows, *similarity_rows],
        max_links_per_ticker=max_links_per_ticker,
        max_links_per_pair=max_links_per_pair,
    )
    conn.executemany(
        """
        INSERT OR REPLACE INTO global_chain_index(
            link_id, link_type, from_ticker, to_ticker, shared_key,
            shared_key_type, from_object_id, to_object_id, weight, confidence,
            evidence_grade, materiality, generic_penalty, recency_score,
            explanation_template
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        [_link_tuple(row) for row in all_rows],
    )
    return CrossCompanyLinkGenerationResult(
        inserted=len(all_rows),
        exact_links=len(exact_rows),
        similarity_links=len(similarity_rows),
        key_count=len(key_stats),
        skipped_generic_keys=skipped_generic_keys,
    )


def refresh_global_key_stats(conn: sqlite3.Connection) -> int:
    """Rebuild IDF-like key statistics from the current global spine tables."""
    conn.row_factory = sqlite3.Row
    conn.execute("DELETE FROM global_key_stats")
    sources = (
        ("factor", "global_factor_spine", "factor_key", "ticker", "object_id", "document_id"),
        ("topic", "global_topic_spine", "topic_key", "ticker", "topic_id", "topic_id"),
        ("metric", "global_metric_spine", "canonical_metric_key", "ticker", "object_id", "document_id"),
        ("entity", "global_entity_spine", "entity_key", "ticker", "object_id", "document_id"),
        ("counterparty", "global_counterparty_spine", "counterparty_key", "ticker", "object_id", "document_id"),
    )
    rows: list[tuple[Any, ...]] = []
    for key_type, table, key_col, ticker_col, object_col, document_col in sources:
        if not _table_exists(conn, table):
            continue
        for row in conn.execute(
            f"""
            SELECT
                {key_col} AS key,
                COUNT(DISTINCT {ticker_col}) AS ticker_count,
                COUNT(DISTINCT {object_col}) AS object_count,
                COUNT(DISTINCT {document_col}) AS document_count
            FROM {table}
            WHERE {key_col} IS NOT NULL AND {key_col} != ''
            GROUP BY {key_col}
            """
        ):
            ticker_count = int(row["ticker_count"] or 0)
            object_count = int(row["object_count"] or 0)
            document_count = int(row["document_count"] or 0)
            idf_score = 1.0 / math.sqrt(max(ticker_count, 1))
            generic = 1 if ticker_count >= 25 or _is_generic_key(str(row["key"])) else 0
            rows.append((key_type, row["key"], ticker_count, object_count, document_count, idf_score, generic))
    conn.executemany(
        """
        INSERT OR REPLACE INTO global_key_stats(
            key_type, key, ticker_count, object_count, document_count,
            idf_score, generic
        )
        VALUES (?, ?, ?, ?, ?, ?, ?)
        """,
        rows,
    )
    return len(rows)


def _exact_key_links(
    conn: sqlite3.Connection,
    *,
    key_stats: Mapping[tuple[str, str], Mapping[str, Any]],
    max_tickers_per_key: int,
    max_links_per_key: int,
) -> tuple[list[dict[str, Any]], int]:
    refs_by_key = _collect_exact_refs(conn)
    links: list[dict[str, Any]] = []
    skipped_generic_keys = 0
    for (key_type, key), refs in sorted(refs_by_key.items()):
        refs_by_ticker = _best_ref_per_ticker(refs)
        if len(refs_by_ticker) < 2:
            continue
        stats = key_stats.get((key_type, key), {})
        generic = bool(stats.get("generic"))
        if generic and len(refs_by_ticker) > max_tickers_per_key:
            skipped_generic_keys += 1
            refs_by_ticker = dict(
                sorted(
                    refs_by_ticker.items(),
                    key=lambda item: (-_base_ref_score(item[1]), item[0]),
                )[:max_tickers_per_key]
            )
        key_links: list[dict[str, Any]] = []
        tickers = sorted(refs_by_ticker)
        for i, from_ticker in enumerate(tickers):
            for to_ticker in tickers[i + 1 :]:
                from_ref = refs_by_ticker[from_ticker]
                to_ref = refs_by_ticker[to_ticker]
                key_links.append(
                    _make_link(
                        link_type=f"shared_{key_type}",
                        shared_key_type=key_type,
                        shared_key=key,
                        from_ref=from_ref,
                        to_ref=to_ref,
                        ticker_count=len(refs_by_ticker),
                    )
                )
        links.extend(
            sorted(key_links, key=lambda row: (-float(row["weight"]), row["from_ticker"], row["to_ticker"]))[
                :max_links_per_key
            ]
        )
    return links, skipped_generic_keys


def _topic_similarity_links(
    conn: sqlite3.Connection,
    *,
    max_refs_per_similarity_token: int,
    min_similarity_score: float,
) -> list[dict[str, Any]]:
    if not _table_exists(conn, "global_topic_spine"):
        return []
    topic_refs = [
        _topic_ref(row)
        for row in conn.execute(
            """
            SELECT topic_id, topic_key, topic_label, topic_summary, ticker,
                   source_object_ids, factor_terms, metric_terms, entity_terms,
                   mechanism_terms, impact_channels, evidence_grade, materiality
            FROM global_topic_spine
            """
        )
    ]
    token_index: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for ref in topic_refs:
        for token in ref["tokens"]:
            token_index[token].append(ref)
    pair_candidates: dict[tuple[str, str], tuple[dict[str, Any], dict[str, Any], float]] = {}
    for refs in token_index.values():
        if len(refs) < 2 or len(refs) > max_refs_per_similarity_token:
            continue
        sorted_refs = sorted(refs, key=lambda ref: (ref["ticker"], ref["object_id"]))
        for i, left in enumerate(sorted_refs):
            for right in sorted_refs[i + 1 :]:
                if left["ticker"] == right["ticker"]:
                    continue
                if left["shared_key"] == right["shared_key"]:
                    continue
                score = _jaccard(left["tokens"], right["tokens"])
                if score < min_similarity_score:
                    continue
                key = tuple(sorted((left["object_id"], right["object_id"])))
                previous = pair_candidates.get(key)
                if previous is None or score > previous[2]:
                    pair_candidates[key] = (left, right, score)
    links: list[dict[str, Any]] = []
    for left, right, score in pair_candidates.values():
        link = _make_link(
            link_type="similar_topic",
            shared_key_type="topic_similarity",
            shared_key=_stable_hash(sorted([left["shared_key"], right["shared_key"]]))[:24],
            from_ref=left,
            to_ref=right,
            ticker_count=2,
        )
        link["weight"] = round(float(link["weight"]) * score, 6)
        link["confidence"] = round(score, 6)
        link["explanation_template"] = "Similar topic language connects {from_ticker} and {to_ticker}."
        links.append(link)
    return links


def _collect_exact_refs(conn: sqlite3.Connection) -> dict[tuple[str, str], list[dict[str, Any]]]:
    refs: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    specs = (
        (
            "factor",
            "global_factor_spine",
            "factor_key AS shared_key, ticker, object_id, factor_label AS label, materiality, evidence_grade",
        ),
        (
            "topic",
            "global_topic_spine",
            "topic_key AS shared_key, ticker, COALESCE(json_extract(source_object_ids, '$[0]'), topic_id) AS object_id, topic_label AS label, materiality, evidence_grade",
        ),
        (
            "metric",
            "global_metric_spine",
            "canonical_metric_key AS shared_key, ticker, object_id, metric_name AS label, confidence AS materiality, NULL AS evidence_grade",
        ),
        (
            "entity",
            "global_entity_spine",
            "entity_key AS shared_key, ticker, object_id, canonical_name AS label, confidence AS materiality, NULL AS evidence_grade",
        ),
        (
            "counterparty",
            "global_counterparty_spine",
            "counterparty_key AS shared_key, ticker, object_id, counterparty_name AS label, materiality, evidence_grade",
        ),
    )
    for key_type, table, columns in specs:
        if not _table_exists(conn, table):
            continue
        for row in conn.execute(f"SELECT {columns} FROM {table} WHERE shared_key IS NOT NULL AND shared_key != ''"):
            key = str(row["shared_key"])
            refs[(key_type, key)].append(
                {
                    "shared_key_type": key_type,
                    "shared_key": key,
                    "ticker": str(row["ticker"]).upper(),
                    "object_id": row["object_id"],
                    "label": row["label"],
                    "materiality": _float(row["materiality"], default=0.5),
                    "evidence_grade": row["evidence_grade"],
                }
            )
    return refs


def _topic_ref(row: sqlite3.Row) -> dict[str, Any]:
    object_ids = _json_list(row["source_object_ids"])
    tokens = _topic_tokens(row)
    return {
        "shared_key_type": "topic",
        "shared_key": str(row["topic_key"]),
        "ticker": str(row["ticker"]).upper(),
        "object_id": object_ids[0] if object_ids else row["topic_id"],
        "label": row["topic_label"],
        "materiality": _float(row["materiality"], default=0.5),
        "evidence_grade": row["evidence_grade"],
        "tokens": tokens,
    }


def _topic_tokens(row: sqlite3.Row) -> frozenset[str]:
    parts = [
        row["topic_label"],
        row["topic_summary"],
        row["factor_terms"],
        row["metric_terms"],
        row["entity_terms"],
        row["mechanism_terms"],
        row["impact_channels"],
    ]
    tokens: set[str] = set()
    for part in parts:
        if part is None:
            continue
        if isinstance(part, str) and part.startswith("["):
            values = _json_list(part)
        else:
            values = [str(part)]
        for value in values:
            for token in re.findall(r"[A-Za-z][A-Za-z0-9_]{2,}", str(value).lower()):
                if token not in _GENERIC_TOKENS:
                    tokens.add(token)
    return frozenset(tokens)


def _best_ref_per_ticker(refs: Iterable[Mapping[str, Any]]) -> dict[str, Mapping[str, Any]]:
    best: dict[str, Mapping[str, Any]] = {}
    for ref in refs:
        ticker = str(ref["ticker"]).upper()
        previous = best.get(ticker)
        if previous is None or _base_ref_score(ref) > _base_ref_score(previous):
            best[ticker] = ref
    return best


def _make_link(
    *,
    link_type: str,
    shared_key_type: str,
    shared_key: str,
    from_ref: Mapping[str, Any],
    to_ref: Mapping[str, Any],
    ticker_count: int,
) -> dict[str, Any]:
    from_ticker = str(from_ref["ticker"]).upper()
    to_ticker = str(to_ref["ticker"]).upper()
    if to_ticker < from_ticker:
        from_ref, to_ref = to_ref, from_ref
        from_ticker, to_ticker = to_ticker, from_ticker
    specificity = 1.0 / math.sqrt(max(ticker_count, 1))
    evidence_score = (_evidence_score(from_ref.get("evidence_grade")) + _evidence_score(to_ref.get("evidence_grade"))) / 2
    materiality = (_float(from_ref.get("materiality"), default=0.5) + _float(to_ref.get("materiality"), default=0.5)) / 2
    generic_penalty = max(0.0, min(0.75, (ticker_count - 10) / 100))
    weight = max(0.0, specificity * evidence_score * materiality * (1.0 - generic_penalty))
    link = {
        "link_type": link_type,
        "from_ticker": from_ticker,
        "to_ticker": to_ticker,
        "shared_key": shared_key,
        "shared_key_type": shared_key_type,
        "from_object_id": from_ref.get("object_id"),
        "to_object_id": to_ref.get("object_id"),
        "weight": round(weight, 6),
        "confidence": round(evidence_score, 6),
        "evidence_grade": _combine_evidence_grade(from_ref.get("evidence_grade"), to_ref.get("evidence_grade")),
        "materiality": round(materiality, 6),
        "generic_penalty": round(generic_penalty, 6),
        "recency_score": None,
        "explanation_template": (
            f"Shared {shared_key_type} '{shared_key}' connects "
            "{from_ticker} and {to_ticker}."
        ),
    }
    link["link_id"] = _link_id(link)
    return link


def _sparsify_links(
    links: Iterable[dict[str, Any]],
    *,
    max_links_per_ticker: int,
    max_links_per_pair: int,
) -> list[dict[str, Any]]:
    sorted_links = sorted(
        links,
        key=lambda row: (
            -float(row["weight"]),
            row["link_type"],
            row["from_ticker"],
            row["to_ticker"],
            row["shared_key_type"],
            row["shared_key"],
        ),
    )
    ticker_counts: dict[str, int] = defaultdict(int)
    pair_counts: dict[tuple[str, str], int] = defaultdict(int)
    selected: list[dict[str, Any]] = []
    for row in sorted_links:
        pair = (row["from_ticker"], row["to_ticker"])
        if pair_counts[pair] >= max_links_per_pair:
            continue
        if ticker_counts[row["from_ticker"]] >= max_links_per_ticker:
            continue
        if ticker_counts[row["to_ticker"]] >= max_links_per_ticker:
            continue
        selected.append(row)
        pair_counts[pair] += 1
        ticker_counts[row["from_ticker"]] += 1
        ticker_counts[row["to_ticker"]] += 1
    return sorted(selected, key=lambda row: row["link_id"])


def _key_stats(conn: sqlite3.Connection) -> dict[tuple[str, str], Mapping[str, Any]]:
    return {
        (str(row["key_type"]), str(row["key"])): dict(row)
        for row in conn.execute("SELECT * FROM global_key_stats")
    }


def _link_tuple(row: Mapping[str, Any]) -> tuple[Any, ...]:
    return (
        row["link_id"],
        row["link_type"],
        row["from_ticker"],
        row["to_ticker"],
        row["shared_key"],
        row["shared_key_type"],
        row.get("from_object_id"),
        row.get("to_object_id"),
        row["weight"],
        row.get("confidence"),
        row.get("evidence_grade"),
        row.get("materiality"),
        row.get("generic_penalty"),
        row.get("recency_score"),
        row.get("explanation_template"),
    )


def _link_id(row: Mapping[str, Any]) -> str:
    return _stable_hash(
        {
            "link_type": row["link_type"],
            "from_ticker": row["from_ticker"],
            "to_ticker": row["to_ticker"],
            "shared_key": row["shared_key"],
            "shared_key_type": row["shared_key_type"],
            "from_object_id": row.get("from_object_id"),
            "to_object_id": row.get("to_object_id"),
        }
    )


def _base_ref_score(ref: Mapping[str, Any]) -> float:
    return _float(ref.get("materiality"), default=0.5) * _evidence_score(ref.get("evidence_grade"))


def _evidence_score(value: Any) -> float:
    text = str(value or "").lower()
    if text in {"high", "strong", "traceable"}:
        return 1.0
    if text in {"medium", "moderate"}:
        return 0.7
    if text in {"low", "weak"}:
        return 0.4
    return 0.6


def _combine_evidence_grade(left: Any, right: Any) -> str:
    score = min(_evidence_score(left), _evidence_score(right))
    if score >= 0.9:
        return "high"
    if score >= 0.6:
        return "medium"
    return "low"


def _is_generic_key(key: str) -> bool:
    tokens = set(re.findall(r"[a-z0-9]+", key.lower()))
    return bool(tokens) and tokens.issubset(_GENERIC_TOKENS)


def _jaccard(left: frozenset[str], right: frozenset[str]) -> float:
    if not left or not right:
        return 0.0
    return len(left & right) / len(left | right)


def _json_list(value: Any) -> list[str]:
    if value is None:
        return []
    try:
        parsed = json.loads(str(value))
    except json.JSONDecodeError:
        parsed = value
    if isinstance(parsed, list):
        return [str(item) for item in parsed if str(item).strip()]
    if isinstance(parsed, str) and parsed.strip():
        return [parsed.strip()]
    return []


def _float(value: Any, *, default: float) -> float:
    if value is None:
        return default
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def _table_exists(conn: sqlite3.Connection, table_name: str) -> bool:
    row = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?",
        (table_name,),
    ).fetchone()
    return row is not None


def _stable_hash(value: Any) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, default=str).encode("utf-8")).hexdigest()

