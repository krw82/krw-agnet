"""Stable semantic identity rules for shared ontology index objects.

The source ontology keeps its original spelling for display and provenance.  This
module only defines the projection used to decide whether two rows carrying the
same global ID describe the same semantic object.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any
import hashlib
import json
import unicodedata


SEMANTIC_IDENTITY_POLICY_VERSION = "semantic-identity/v2"

_UNKNOWN_TICKER_VALUES = frozenset({"", "UNKNOWN", "N/A", "NA", "NONE", "NULL"})
GLOBAL_TAXONOMY_TERM_PREFIX = "term:"
GLOBAL_CANONICAL_ENTITY_PREFIXES = (
    "entity:factor:",
    "entity:benchmark:",
    "entity:regulator:",
)

_PRESENTATION_ONLY_FIELDS: Mapping[str, frozenset[str]] = {
    "CanonicalEntity": frozenset({"aliases"}),
    "TaxonomyTerm": frozenset({"aliases"}),
}

_NORMALIZED_IDENTITY_TEXT_FIELDS: Mapping[str, tuple[str, ...]] = {
    "CanonicalEntity": ("canonical_name",),
    "TaxonomyTerm": ("display_name",),
}

OBJECT_OCCURRENCE_ONLY_FIELDS = frozenset(
    {
        "artifact_key",
        "artifact_path",
        "company",
        "company_name",
        "confidence",
        "doc_type_key",
        "document_id",
        "document_type",
        "evidence",
        "evidence_refs",
        "filing_date",
        "period",
        "review_status",
        "section_name",
        "source_document_id",
        "source_path",
        "source_quote",
        "source_span",
        "supporting_evidence",
        "ticker",
        "ticker_scope",
    }
)


def normalize_identity_text(value: Any) -> str:
    """Normalize human spelling without changing the stored source value."""
    text = unicodedata.normalize("NFKC", str(value or "")).casefold().strip()
    parts: list[str] = []
    pending_separator = False
    for character in text:
        if character.isalnum():
            if pending_separator and parts:
                parts.append("_")
            parts.append(character)
            pending_separator = False
        else:
            pending_separator = bool(parts)
    return "".join(parts).strip("_")


def effective_ticker(value: Any, *, fallback: str) -> str:
    """Return a usable occurrence ticker, replacing source placeholders."""
    candidate = str(value or "").strip().upper()
    resolved_fallback = str(fallback or "").strip().upper()
    if not resolved_fallback or resolved_fallback in _UNKNOWN_TICKER_VALUES:
        raise ValueError("a concrete fallback ticker is required")
    return resolved_fallback if candidate in _UNKNOWN_TICKER_VALUES else candidate


def is_global_object_identity(
    object_id: Any,
    object_type: Any,
    payload: Mapping[str, Any] | None = None,
) -> bool:
    """Whether an object ID is intentionally shared across company shards."""
    resolved_id = str(object_id or "")
    resolved_type = str(
        object_type or (payload or {}).get("object_type") or (payload or {}).get("type") or ""
    )
    if resolved_type == "TaxonomyTerm":
        return resolved_id.startswith(GLOBAL_TAXONOMY_TERM_PREFIX)
    if resolved_type == "CanonicalEntity":
        return resolved_id.startswith(GLOBAL_CANONICAL_ENTITY_PREFIXES)
    return False


def identity_contains_ticker(identity: Any, ticker: str) -> bool:
    resolved_identity = str(identity or "").upper()
    resolved_ticker = str(ticker or "").strip().upper()
    if not resolved_identity or not resolved_ticker:
        return False
    return f":{resolved_ticker}:" in f":{resolved_identity}:"


def project_local_identity(identity: Any, *, ticker: str) -> str:
    """Namespace an occurrence ID only when it is not already ticker-scoped."""
    raw_identity = str(identity or "").strip()
    resolved_ticker = effective_ticker(ticker, fallback=ticker)
    if not raw_identity:
        raise ValueError("identity is required")
    if identity_contains_ticker(raw_identity, resolved_ticker):
        return raw_identity
    return f"scoped:{resolved_ticker}:{raw_identity}"


def project_object_identity(
    object_id: Any,
    object_type: Any,
    *,
    ticker: str,
    payload: Mapping[str, Any] | None = None,
) -> str:
    """Project a source object ID into the global index identity domain."""
    raw_object_id = str(object_id or "").strip()
    if is_global_object_identity(raw_object_id, object_type, payload):
        return raw_object_id
    return project_local_identity(raw_object_id, ticker=ticker)


def semantic_object_projection(
    payload: Mapping[str, Any],
    *,
    object_id: str,
    object_type: str,
    occurrence_only_fields: frozenset[str],
) -> dict[str, Any]:
    """Return the hard semantic identity projected from one source object.

    CanonicalEntity and TaxonomyTerm aliases are additive presentation/search
    metadata. They are deliberately excluded from hard identity, while their
    display-bearing name is compared with the same case/separator-insensitive
    semantics used by global IDs. Other object types retain their strict
    projection.
    """
    projected = {
        str(key): value for key, value in payload.items() if str(key) not in occurrence_only_fields
    }
    resolved_type = str(projected.get("object_type") or projected.get("type") or object_type or "")
    for field in _PRESENTATION_ONLY_FIELDS.get(resolved_type, ()):
        projected.pop(field, None)
    for field in _NORMALIZED_IDENTITY_TEXT_FIELDS.get(resolved_type, ()):
        if field in projected:
            projected[field] = normalize_identity_text(projected[field])
    projected.setdefault("id", object_id)
    projected.setdefault("type", object_type)
    return projected


def semantic_object_hash(
    payload: Mapping[str, Any],
    *,
    object_id: str,
    object_type: str,
) -> str:
    projected = semantic_object_projection(
        payload,
        object_id=object_id,
        object_type=object_type,
        occurrence_only_fields=OBJECT_OCCURRENCE_ONLY_FIELDS,
    )
    projected["semantic_identity_policy_version"] = SEMANTIC_IDENTITY_POLICY_VERSION
    encoded = json.dumps(projected, sort_keys=True, default=str).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()
