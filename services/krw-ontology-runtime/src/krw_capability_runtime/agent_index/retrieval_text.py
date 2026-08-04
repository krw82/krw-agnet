"""Index-only retrieval text expansion for ontology objects.

Canonical JSONL artifacts remain the source of truth. This module builds
serving-only text for the SQLite agent index by combining an object's own
fields with nearby evidence, claims, metrics, entities, and graph context.
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass
import re
from typing import Any, Iterable

RETRIEVAL_TEXT_BUILDER_VERSION = "1.0.0-alpha.1"

MAX_FIELD_CHARS = 4_000
MAX_QUOTE_CHARS = 1_200
MAX_SUPPORT_OBJECTS = 12
MAX_RELATED_OBJECTS = 16


@dataclass(frozen=True)
class RetrievalText:
    text_self: str
    text_support: str
    text_related: str
    text_entities: str
    text_aliases: str
    compact_text: str

    @property
    def joined(self) -> str:
        return _dedupe_text(
            (
                self.text_self,
                self.text_support,
                self.text_related,
                self.text_entities,
                self.text_aliases,
                self.compact_text,
            )
        )


SELF_TEXT_FIELDS_BY_TYPE: dict[str, tuple[str, ...]] = {
    "RunManifest": (
        "run_id",
        "pipeline_version",
        "ontology_schema_version",
        "ontology_registry_version",
        "model",
    ),
    "OntologyRegistrySnapshot": (
        "registry_version",
        "canonical_artifacts",
        "text_fields_by_type",
    ),
    "ValidationReport": ("validation_scope", "summary"),
    "TaxonomyTerm": ("taxonomy", "term_type", "canonical_name", "display_name", "aliases"),
    "SourceDocument": ("ticker", "company_name", "accession_number", "source_url"),
    "SourceLocation": ("source_boundary", "section_name", "section_path", "source_span_id"),
    "SourceTable": ("section_name", "caption"),
    "SourceTableCell": ("raw_text", "normalized_text"),
    "SourceSpan": ("text",),
    "EvidenceQuote": ("quote_text", "quote_type", "section_name", "language_signals"),
    "LanguageSignal": ("signal_text", "polarity", "modality", "certainty"),
    "SupportLink": (
        "support_type",
        "support_role",
        "stance",
        "evidence_strength",
        "support_strength",
        "from_id",
        "to_id",
        "explanation",
    ),
    "CanonicalEntity": ("entity_type", "canonical_name", "aliases", "ticker_scope"),
    "EntityMention": ("mention_text", "source_object_type", "canonical_entity_id"),
    "ResearchClaim": (
        "claim_text",
        "normalized_claim_text",
        "claim_type",
        "theme_hint",
        "factor_hint",
        "activity_hint",
        "benchmark_hint",
        "impact_channels",
        "effect_direction",
        "related_metrics",
        "materiality_hint",
        "materiality_basis",
        "time_horizon",
        "sector_hint",
        "object_type_hints",
    ),
    "MetricObservation": (
        "metric_name",
        "metric_term_id",
        "value",
        "unit",
        "scale",
        "period",
        "fiscal_year",
        "fiscal_quarter",
        "fiscal_period",
        "period_end",
        "period_end_date",
        "period_type",
        "source_type",
        "normalization",
        "dimensions",
    ),
    "Calculation": (
        "calculation_type",
        "formula",
        "calculation_method",
        "validation_status",
        "output_metric_id",
        "input_metric_ids",
    ),
    "BusinessFactor": (
        "name",
        "description",
        "factor_roles",
        "category",
        "occurrence_status",
        "company_polarity",
        "affected_channels",
        "materiality_hint",
        "materiality_basis",
        "specificity_score",
        "boilerplate_score",
    ),
    "AgreementTerm": (
        "name",
        "agreement_type",
        "agreement_subtype",
        "counterparty",
        "economic_role",
        "pricing_mechanism",
        "volume_commitment",
        "minimum_commitment",
        "maturity_date",
        "covenants",
        "affected_channels",
    ),
    "BusinessEvent": (
        "name",
        "description",
        "event_type",
        "event_subtype",
        "event_status",
        "date_expression",
        "affected_channels",
    ),
    "BusinessActivity": (
        "name",
        "activity_type",
        "description",
        "revenue_relevance",
        "cost_relevance",
        "capex_relevance",
        "sector_tags",
        "related_metrics",
        "related_factor_term_ids",
    ),
    "ExternalFactorExposure": (
        "factor",
        "external_factor_term_id",
        "factor_category",
        "benchmark",
        "benchmark_entity_id",
        "exposure_type",
        "direction",
        "impact_channel",
        "impact_channels",
        "affected_channels",
        "effect_direction",
        "mechanism",
        "scenario_effects",
        "pass_through_mechanism",
        "offsetting_factors",
        "evidence_grade",
        "materiality",
        "materiality_basis",
    ),
    "AssumptionCandidate": (
        "name",
        "assumption_text",
        "value_hint",
        "assumption_type",
        "basis_type",
        "caveats",
    ),
    "XBRLFact": (
        "taxonomy_tag",
        "safe_taxonomy_tag",
        "concept",
        "context_ref",
        "unit",
        "period_start",
        "period_end",
    ),
    "CompanyBusinessProfile": (
        "sector",
        "business_model_summary",
        "primary_business_activities",
        "primary_revenue_sources",
        "primary_cost_sources",
        "key_external_factors",
        "key_metrics",
        "key_uncertainties",
        "query_vocabulary",
    ),
    "TemporalLink": (
        "from_object_id",
        "to_object_id",
        "relation",
        "rationale",
        "from_period",
        "to_period",
    ),
    "TrendObservation": (
        "subject",
        "metric_or_factor",
        "direction",
        "magnitude_text",
        "interpretation",
    ),
    "ChangeEvent": ("event_type", "event_date", "description", "affected_objects"),
}

EXPLICIT_SUPPORT_FIELDS = (
    "supported_by_claims",
    "supported_by_quotes",
    "supported_by_objects",
    "source_object_ids",
    "source_fact_ids",
    "source_metric_ids",
    "input_metric_ids",
)

EXPLICIT_RELATED_FIELDS = (
    "related_exposure_ids",
    "related_external_factor_exposure_ids",
    "related_activity_ids",
    "related_business_activity_ids",
    "related_event_ids",
    "related_metric_ids",
    "related_entity_ids",
    "affected_objects",
)


class ObjectLookup:
    """In-memory helper used only while building the SQLite index."""

    def __init__(
        self,
        objects_by_id: dict[str, dict[str, Any]],
        support_links: list[dict[str, Any]],
        edges: list[dict[str, Any]],
    ) -> None:
        self.objects_by_id = objects_by_id
        self.support_by_target: dict[str, list[dict[str, Any]]] = defaultdict(list)
        self.edges_by_from: dict[str, list[dict[str, Any]]] = defaultdict(list)
        self.edges_by_to: dict[str, list[dict[str, Any]]] = defaultdict(list)

        for link in support_links:
            target_id = _support_target_id(link)
            if target_id:
                self.support_by_target[target_id].append(link)

        for edge in edges:
            from_id = edge.get("from_id") or edge.get("from_object_id")
            to_id = edge.get("to_id") or edge.get("to_object_id")
            if from_id:
                self.edges_by_from[str(from_id)].append(edge)
            if to_id:
                self.edges_by_to[str(to_id)].append(edge)

    def get(self, object_id: Any) -> dict[str, Any] | None:
        if not object_id:
            return None
        return self.objects_by_id.get(str(object_id))

    def support_sources(
        self,
        obj: dict[str, Any],
        *,
        limit: int = MAX_SUPPORT_OBJECTS,
    ) -> list[dict[str, Any]]:
        object_id = obj.get("id")
        output: list[dict[str, Any]] = []

        if object_id:
            for link in self.support_by_target.get(str(object_id), []):
                source = self.get(_support_source_id(link))
                if source:
                    output.append(source)

        for field in EXPLICIT_SUPPORT_FIELDS:
            for ref_id in _as_list(obj.get(field)):
                source = self.get(ref_id)
                if source:
                    output.append(source)

        return _dedupe_objects(output)[:limit]

    def semantic_neighbors(
        self,
        obj: dict[str, Any],
        *,
        limit: int = MAX_RELATED_OBJECTS,
    ) -> list[dict[str, Any]]:
        object_id = obj.get("id")
        if not object_id:
            return []

        output: list[dict[str, Any]] = []
        for edge in self.edges_by_from.get(str(object_id), []):
            neighbor = self.get(edge.get("to_id") or edge.get("to_object_id"))
            if neighbor:
                output.append(neighbor)
        for edge in self.edges_by_to.get(str(object_id), []):
            neighbor = self.get(edge.get("from_id") or edge.get("from_object_id"))
            if neighbor:
                output.append(neighbor)

        for field in EXPLICIT_RELATED_FIELDS:
            for ref_id in _as_list(obj.get(field)):
                neighbor = self.get(ref_id)
                if neighbor:
                    output.append(neighbor)

        return _dedupe_objects(output)[:limit]


def build_retrieval_text(
    obj: dict[str, Any],
    lookup: ObjectLookup,
    taxonomy_by_id: dict[str, dict[str, Any]] | None = None,
) -> RetrievalText:
    """Build index-only text fields for one ontology object."""
    return RetrievalText(
        text_self=own_text(obj),
        text_support=support_text(obj, lookup),
        text_related=related_text(obj, lookup),
        text_entities=entity_text(obj, lookup),
        text_aliases=alias_text(obj, taxonomy_by_id or {}),
        compact_text=build_compact_text(obj),
    )


def fallback_retrieval_text(obj: dict[str, Any]) -> RetrievalText:
    text = own_text(obj)
    compact = build_compact_text(obj) or text
    return RetrievalText(
        text_self=text,
        text_support="",
        text_related="",
        text_entities="",
        text_aliases="",
        compact_text=compact,
    )


def own_text(obj: dict[str, Any]) -> str:
    object_type = obj.get("type") or obj.get("object_type")
    fields = SELF_TEXT_FIELDS_BY_TYPE.get(str(object_type), ())
    parts: list[str] = [str(object_type or "")]
    for field in fields:
        parts.append(_clean_text(obj.get(field)))
    for extra_field in ("related_metrics", "affects", "impact_channels", "affected_channels"):
        parts.append(_clean_text(obj.get(extra_field)))
    return _truncate(_dedupe_text(parts))


def support_text(obj: dict[str, Any], lookup: ObjectLookup) -> str:
    parts: list[str] = []
    for source in lookup.support_sources(obj):
        source_type = source.get("type") or source.get("object_type")
        if source_type == "ResearchClaim":
            parts.extend(
                [
                    source.get("claim_text", ""),
                    source.get("normalized_claim_text", ""),
                    source.get("theme_hint", ""),
                    source.get("factor_hint", ""),
                    source.get("activity_hint", ""),
                    source.get("benchmark_hint", ""),
                    source.get("impact_channels", ""),
                    source.get("related_metrics", ""),
                    source.get("object_type_hints", ""),
                    source.get("materiality_basis", ""),
                ]
            )
            for quote in lookup.support_sources(source, limit=6):
                if (quote.get("type") or quote.get("object_type")) == "EvidenceQuote":
                    parts.append(str(quote.get("quote_text", ""))[:MAX_QUOTE_CHARS])
                    parts.append(quote.get("quote_type", ""))
                    parts.append(quote.get("section_name", ""))
        elif source_type == "EvidenceQuote":
            parts.append(str(source.get("quote_text", ""))[:MAX_QUOTE_CHARS])
            parts.append(source.get("quote_type", ""))
            parts.append(source.get("section_name", ""))
        elif source_type == "MetricObservation":
            parts.extend(
                [
                    source.get("metric_name", ""),
                    source.get("metric_term_id", ""),
                    source.get("period", ""),
                    source.get("source_type", ""),
                    source.get("dimensions", ""),
                ]
            )
        else:
            parts.append(own_text(source))
    return _truncate(_dedupe_text(parts))


def related_text(obj: dict[str, Any], lookup: ObjectLookup) -> str:
    parts: list[str] = []
    for neighbor in lookup.semantic_neighbors(obj):
        neighbor_type = neighbor.get("type") or neighbor.get("object_type")
        if neighbor_type in {"SupportLink", "Edge"}:
            continue
        if neighbor_type == "ExternalFactorExposure":
            parts.extend(
                [
                    neighbor.get("factor", ""),
                    neighbor.get("external_factor_term_id", ""),
                    neighbor.get("benchmark", ""),
                    neighbor.get("benchmark_entity_id", ""),
                    neighbor.get("mechanism", ""),
                    neighbor.get("impact_channel", ""),
                    neighbor.get("affected_channels", ""),
                    neighbor.get("scenario_effects", ""),
                    neighbor.get("pass_through_mechanism", ""),
                    neighbor.get("offsetting_factors", ""),
                ]
            )
        elif neighbor_type in {"CanonicalEntity", "EntityMention"}:
            parts.append(entity_text(neighbor, lookup))
        else:
            parts.append(own_text(neighbor))
    return _truncate(_dedupe_text(parts))


def entity_text(obj: dict[str, Any], lookup: ObjectLookup | None = None) -> str:
    parts = [
        obj.get("canonical_name", ""),
        obj.get("entity_type", ""),
        obj.get("aliases", ""),
        obj.get("mention_text", ""),
        obj.get("sector_tags", ""),
    ]
    if lookup:
        for ref_id in _as_list(obj.get("canonical_entity_id")) + _as_list(obj.get("related_entity_ids")):
            entity = lookup.get(ref_id)
            if entity:
                parts.extend(
                    [
                        entity.get("canonical_name", ""),
                        entity.get("entity_type", ""),
                        entity.get("aliases", ""),
                    ]
                )
    return _truncate(_dedupe_text(parts), limit=1_500)


def alias_text(obj: dict[str, Any], taxonomy_by_id: dict[str, dict[str, Any]]) -> str:
    parts: list[str] = []
    for field in (
        "external_factor_term_id",
        "metric_term_id",
        "related_factor_term_ids",
        "related_metric_term_ids",
        "category_term_id",
    ):
        for term_id in _as_list(obj.get(field)):
            term = taxonomy_by_id.get(str(term_id))
            if term:
                parts.extend(
                    [
                        term.get("term_key", ""),
                        term.get("canonical_name", ""),
                        term.get("display_name", ""),
                        term.get("aliases", ""),
                    ]
                )
    return _truncate(_dedupe_text(parts), limit=2_000)


def build_compact_text(obj: dict[str, Any]) -> str:
    object_type = obj.get("type") or obj.get("object_type")
    if object_type == "MetricObservation":
        return format_metric_compact(obj)
    if object_type == "BusinessFactor":
        return _clean_text(
            f"{obj.get('name', '')} | roles={_clean_text(obj.get('factor_roles'))} | "
            f"channels={_clean_text(obj.get('affected_channels'))} | {obj.get('description', '')}"
        )
    if object_type == "ExternalFactorExposure":
        return _clean_text(
            f"{obj.get('factor') or obj.get('external_factor_term_id') or ''} | "
            f"{obj.get('benchmark') or obj.get('benchmark_entity_id') or ''} | "
            f"{obj.get('mechanism', '')} | {obj.get('scenario_effects', '')}"
        )
    if object_type == "BusinessEvent":
        return _clean_text(
            f"{obj.get('name', '')} | {obj.get('event_type', '')} | "
            f"{obj.get('event_status', '')} | {obj.get('date_expression', '')}"
        )
    if object_type == "AgreementTerm":
        return _clean_text(
            f"{obj.get('name', '')} | {obj.get('agreement_type', '')} | "
            f"{obj.get('counterparty', '')} | {obj.get('maturity_date', '')} | "
            f"{obj.get('pricing_mechanism', '')}"
        )
    return _display_text(obj)


def format_metric_compact(obj: dict[str, Any]) -> str:
    metric_name = obj.get("metric_name") or obj.get("metric_term_id") or "metric"
    period = obj.get("period") or _format_fiscal_period(obj)
    value = format_number(obj.get("value"), obj.get("unit"))
    source_type = obj.get("source_type") or obj.get("normalization") or ""
    period_end = obj.get("period_end") or obj.get("period_end_date")
    dimensions = obj.get("dimensions") or {}

    dim_text = ""
    if isinstance(dimensions, dict):
        useful_dims = [
            f"{key}={value}"
            for key, value in dimensions.items()
            if value not in (None, "", "null")
        ]
        if useful_dims:
            dim_text = " | " + ", ".join(useful_dims)

    source_text = f" ({source_type})" if source_type else ""
    end_text = f" | period_end={period_end}" if period_end else ""
    return f"{period} {metric_name}: {value}{source_text}{dim_text}{end_text}"


def format_number(value: Any, unit: str | None = None) -> str:
    try:
        number = float(value)
    except (TypeError, ValueError):
        return str(value)

    unit = unit or ""
    if unit.upper() == "USD":
        abs_number = abs(number)
        if abs_number >= 1_000_000_000:
            return f"${number / 1_000_000_000:.1f}B"
        if abs_number >= 1_000_000:
            return f"${number / 1_000_000:.1f}M"
        if abs_number >= 1_000:
            return f"${number / 1_000:.1f}K"
        return f"${number:,.0f}"
    if unit.lower() in {"percent", "%"}:
        return f"{number:.1f}%"
    return f"{number:,.2f} {unit}".strip()


def _format_fiscal_period(obj: dict[str, Any]) -> str:
    fiscal_year = obj.get("fiscal_year")
    fiscal_quarter = obj.get("fiscal_quarter")
    if fiscal_year and fiscal_quarter:
        return f"FY{fiscal_year}Q{fiscal_quarter}"
    if fiscal_year:
        return f"FY{fiscal_year}"
    return obj.get("fiscal_period") or obj.get("period_label") or "period unknown"


def _display_text(obj: dict[str, Any]) -> str:
    for key in (
        "claim_text",
        "quote_text",
        "description",
        "mechanism",
        "business_model_summary",
        "interpretation",
        "assumption_text",
        "text",
        "raw_text",
    ):
        value = obj.get(key)
        if value:
            return str(value)
    return str(obj.get("name") or obj.get("id") or "")


def _support_source_id(link: dict[str, Any]) -> Any:
    return (
        link.get("support_object_id")
        or link.get("from_id")
        or link.get("source_id")
        or link.get("source_object_id")
    )


def _support_target_id(link: dict[str, Any]) -> Any:
    return link.get("target_object_id") or link.get("to_id") or link.get("target_id")


def _as_list(value: Any) -> list[Any]:
    if value is None:
        return []
    if isinstance(value, list):
        return value
    if isinstance(value, tuple | set):
        return list(value)
    return [value]


def _clean_text(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, dict):
        return " ".join(f"{key} {_clean_text(item)}" for key, item in value.items())
    if isinstance(value, (list, tuple, set)):
        return " ".join(_clean_text(item) for item in value)
    text = str(value)
    spaced = re.sub(r"[_\\-]+", " ", text)
    if spaced != text:
        text = f"{text} {spaced}"
    return re.sub(r"\s+", " ", text).strip()


def _dedupe_text(parts: Iterable[Any]) -> str:
    seen: set[str] = set()
    output: list[str] = []
    for part in parts:
        text = _clean_text(part)
        if not text:
            continue
        key = text.lower()
        if key in seen:
            continue
        seen.add(key)
        output.append(text)
    return " ".join(output)


def _dedupe_objects(objects: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    seen: set[str] = set()
    output: list[dict[str, Any]] = []
    for obj in objects:
        object_id = obj.get("id")
        if not object_id or object_id in seen:
            continue
        seen.add(object_id)
        output.append(obj)
    return output


def _truncate(text: str, limit: int = MAX_FIELD_CHARS) -> str:
    text = _clean_text(text)
    if len(text) <= limit:
        return text
    return text[:limit].rsplit(" ", 1)[0]
