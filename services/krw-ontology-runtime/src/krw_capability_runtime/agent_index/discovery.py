"""Evidence-derived company topic discovery helpers."""

from __future__ import annotations

from dataclasses import dataclass
import json
import re
from typing import Any, Mapping

COMPANY_TOPIC_OBJECT_TYPES = {
    "ExternalFactorExposure",
    "BusinessFactor",
    "BusinessActivity",
    "BusinessEvent",
    "AgreementTerm",
    "CompanyBusinessProfile",
    "RiskFactor",
    "GrowthDriver",
    "TrendObservation",
    "ChangeEvent",
}

_DIRECT_EVIDENCE_TYPES = {"ExternalFactorExposure"}
_STRONG_EVIDENCE_TYPES = {"BusinessFactor", "RiskFactor", "GrowthDriver", "AgreementTerm"}

_GENERIC_PATTERNS: dict[str, tuple[str, ...]] = {
    "exposure": ("exposure", "exposures", "exposed"),
    "risk": ("risk", "risks", "uncertainty", "uncertain"),
    "impact": ("impact", "impacts", "affect", "affects", "effect", "effects"),
    "business": ("business", "company", "operations", "operating"),
    "market_volatility": ("market volatility", "volatility", "volatile"),
    "geopolitical": ("geopolitical", "geopolitics", "political instability"),
}

_IMPACT_CHANNEL_PATTERNS: dict[str, tuple[str, ...]] = {
    "revenue": ("revenue", "revenues", "sales", "net sales"),
    "gross_margin": ("gross margin", "margin", "margins"),
    "operating_margin": ("operating margin", "operating income"),
    "cost_of_revenue": ("cost of revenue", "cost of sales", "input cost", "costs", "cost"),
    "cash_flow": ("cash flow", "cash flows", "operating cash flow", "ocf"),
    "capex": ("capex", "capital expenditure", "capital expenditures", "project cost"),
    "liquidity": ("liquidity", "funding", "financing", "capital access"),
    "project_timing": ("project timing", "delay", "delays", "schedule", "timeline", "commercial operation"),
    "customer_demand": ("customer demand", "demand", "orders", "bookings"),
}

_CORE_DOMAIN_PATTERNS: dict[str, tuple[str, ...]] = {
    "commodity_price": ("commodity price", "commodity prices", "commodities", "commodity"),
    "lng_market_price": ("lng", "liquefied natural gas", "lng market", "lng price", "lng prices", "lng sales", "액화천연가스"),
    "natural_gas_price": ("natural gas", "gas price", "gas prices", "feed gas", "henry hub", "ttf", "천연가스"),
    "crude_oil_price": ("crude oil", "oil price", "oil prices", "brent", "wti", "원유", "유가"),
    "refined_product_price": ("refined product", "refining margin", "diesel", "gasoline"),
    "shipping_chokepoint": (
        "shipping chokepoint",
        "physical chokepoint",
        "chokepoint",
        "panama canal",
        "suez canal",
        "red sea",
        "strait of hormuz",
        "hormuz",
        "호르무즈",
        "해협",
        "canal",
        "shipping route",
        "vessel",
        "port congestion",
    ),
    "supply_chain": ("supply chain", "supplier", "suppliers", "component", "components", "shortage", "inventory"),
    "export_restriction": ("export control", "export controls", "export restriction", "license requirement", "license", "china restriction"),
    "credit_market": ("credit loss", "credit losses", "deposit", "interest rate", "loan", "loans", "funding cost"),
    "data_center_demand": ("data center", "datacenter", "accelerated computing", "ai demand", "artificial intelligence"),
}

_MECHANISM_PATTERNS: dict[str, tuple[str, ...]] = {
    "shipping_constraint": ("shipping", "vessel", "canal", "port", "chokepoint", "red sea", "suez", "panama", "hormuz"),
    "physical_supply_chokepoint": ("physical chokepoint", "supply chokepoint", "bottleneck", "constraint", "congestion"),
    "feed_gas_procurement": ("feed gas", "procurement", "natural gas supply"),
    "lng_sales": ("lng sales", "liquefaction", "take-or-pay", "take or pay", "spa", "sale and purchase agreement"),
    "supplier_disruption": ("supply chain", "supplier", "component", "shortage", "inventory", "manufacturing"),
    "regulatory_restriction": ("regulatory", "restriction", "license", "export control", "approval", "permit", "ferc", "doe"),
    "capital_access": ("capital access", "financing", "funding", "debt", "equity", "lender"),
}

_SECTOR_PATTERNS: dict[str, tuple[str, ...]] = {
    "energy_lng": ("lng", "liquefied natural gas", "liquefaction", "feed gas"),
    "energy_oil_gas": ("oil", "natural gas", "upstream", "downstream", "refining", "hydrocarbon"),
    "technology_hardware": (
        "semiconductor",
        "gpu",
        "hbm",
        "high bandwidth memory",
        "memory chip",
        "memory price",
        "hardware",
        "component",
        "data center",
        "accelerated computing",
    ),
    "consumer_devices": ("consumer electronics", "device", "devices", "smartphone", "tablet", "wearable"),
    "financials": ("bank", "deposit", "loan", "credit", "markets", "trading"),
}

_ANCHOR_PATTERNS: dict[str, tuple[str, ...]] = {
    "hbm": ("hbm", "high bandwidth memory"),
    "gpu": ("gpu", "gpus"),
    "semiconductor_memory": ("semiconductor memory", "memory chip", "memory chips", "memory price"),
    "semiconductor": ("semiconductor", "semiconductors"),
    "henry_hub": ("henry hub",),
    "feed_gas": ("feed gas",),
    "natural_gas": ("natural gas",),
    "lng": ("lng", "liquefied natural gas"),
    "spa": ("spa", "sale and purchase agreement", "sale and purchase agreements"),
    "take_or_pay": ("take or pay", "take-or-pay"),
    "termination": ("termination", "terminate", "terminated", "cancellation", "cancel", "cancelled", "canceled"),
    "debt_acceleration": ("debt acceleration", "accelerate debt", "acceleration of debt", "accelerated debt"),
    "ferc": ("ferc",),
    "doe": ("doe",),
}

_PREDICATE_PATTERNS: dict[str, tuple[str, ...]] = {
    "growth_driver": (
        "growth",
        "driver",
        "drivers",
        "contribute",
        "contributes",
        "contributed",
        "contribution",
        "demand",
        "strong demand",
        "higher demand",
    ),
    "direct_exposure": ("direct exposure", "directly exposed", "directly affect", "directly affects", "direct"),
    "price_volatility": ("price cycle", "price volatility", "price fluctuation", "price fluctuations", "volatility", "volatile"),
    "scenario_change": (
        "increase",
        "increased",
        "decrease",
        "decreased",
        "higher",
        "lower",
        "rise",
        "rises",
        "fall",
        "falls",
        "down",
        "up",
    ),
    "contract_termination": ("termination", "terminate", "terminated", "cancel", "cancelled", "canceled", "cancellation"),
    "debt_acceleration": ("debt acceleration", "accelerate", "accelerated", "acceleration"),
}

_CONTEXT_PATTERNS: dict[str, tuple[str, ...]] = {
    "region": ("americas", "europe", "greater china", "china", "asia", "japan"),
    "quarter": ("quarter", "q1", "q2", "q3", "q4"),
    "segment": ("segment", "segments", "business segment"),
    "seasonality": ("seasonality", "seasonal"),
}

_CORE_PARENT_TERMS: dict[str, set[str]] = {
    "commodity_price_volatility": {"commodity_price"},
    "lng_market_price": {"commodity_price", "natural_gas_price"},
    "natural_gas_price": {"commodity_price"},
    "crude_oil_price": {"commodity_price"},
    "refined_product_price": {"commodity_price", "crude_oil_price"},
}

_TIER_RANK = {
    "orphan": 0,
    "insufficient": 0,
    "untraced_related": 1,
    "indirect": 1,
    "related": 2,
    "traceable_related": 3,
    "strong_related": 3,
    "untraced_direct_candidate": 3,
    "direct": 4,
    "traceable_direct": 5,
}


@dataclass(frozen=True)
class QueryFrame:
    raw_query: str
    normalized_query: str
    query_type: str
    must_for_direct: frozenset[str]
    should_for_direct: frozenset[str]
    context_facets: frozenset[str]
    predicate_terms: frozenset[str]
    core_domain_terms: frozenset[str]
    sector_terms: frozenset[str]
    mechanism_terms: frozenset[str]
    impact_channels: frozenset[str]
    generic_terms: frozenset[str]
    required_for_direct: frozenset[str]
    premises: tuple[dict[str, Any], ...]

    def as_dict(self) -> dict[str, Any]:
        return {
            "raw_query": self.raw_query,
            "query_type": self.query_type,
            "premises": list(self.premises),
            "must_for_direct": sorted(self.must_for_direct),
            "should_for_direct": sorted(self.should_for_direct),
            "context_facets": sorted(self.context_facets),
            "predicate_terms": sorted(self.predicate_terms),
            "core_domain_terms": sorted(self.core_domain_terms),
            "sector_terms": sorted(self.sector_terms),
            "mechanism_terms": sorted(self.mechanism_terms),
            "impact_channels": sorted(self.impact_channels),
            "generic_terms": sorted(self.generic_terms),
            "required_for_direct": sorted(self.required_for_direct),
        }


@dataclass(frozen=True)
class EvidenceFrame:
    object_id: str
    object_type: str
    ticker: str
    topic_id: str | None
    anchor_terms: frozenset[str]
    predicate_terms: frozenset[str]
    context_facets: frozenset[str]
    core_domain_terms: frozenset[str]
    sector_terms: frozenset[str]
    mechanism_terms: frozenset[str]
    impact_channels: frozenset[str]
    generic_terms: frozenset[str]
    evidence_strength: str
    support_quote_count: int
    support_claim_count: int
    specificity_score: float
    materiality_hint: str | None


def build_query_frame(query: str) -> QueryFrame:
    raw_query = query or ""
    normalized = _normalize(raw_query)
    facets = _extract_facets(normalized, is_query=True)
    core_terms = set(facets["core_domain_terms"])
    anchor_terms = set(facets["anchor_terms"])
    predicate_terms = set(facets["predicate_terms"])

    has_volatility = _has_any(normalized, ("volatility", "volatile"))
    has_commodity_context = bool(core_terms & {"commodity_price", "lng_market_price", "natural_gas_price", "crude_oil_price", "refined_product_price"})
    if has_volatility and has_commodity_context:
        core_terms.add("commodity_price_volatility")
        facets["generic_terms"].discard("market_volatility")

    query_type = _infer_query_frame_type(raw_query, normalized, facets)
    must_for_direct = _must_for_direct(
        query_type=query_type,
        anchor_terms=anchor_terms,
        core_terms=core_terms,
        mechanism_terms=set(facets["mechanism_terms"]),
    )
    should_for_direct = set(facets["impact_channels"]) | predicate_terms
    context_facets = set(facets["sector_terms"]) | set(facets["context_facets"])

    premises = tuple(_premises_from_terms(core_terms, facets["mechanism_terms"]))
    return QueryFrame(
        raw_query=raw_query,
        normalized_query=normalized,
        query_type=query_type,
        must_for_direct=frozenset(must_for_direct),
        should_for_direct=frozenset(should_for_direct),
        context_facets=frozenset(context_facets),
        predicate_terms=frozenset(predicate_terms),
        core_domain_terms=frozenset(core_terms),
        sector_terms=frozenset(facets["sector_terms"]),
        mechanism_terms=frozenset(facets["mechanism_terms"]),
        impact_channels=frozenset(facets["impact_channels"]),
        generic_terms=frozenset(facets["generic_terms"]),
        required_for_direct=frozenset(must_for_direct),
        premises=premises,
    )


def build_evidence_frame(topic: Mapping[str, Any]) -> EvidenceFrame:
    text = _topic_match_text(topic)
    facets = _extract_facets(text, is_query=False)
    evidence_strength = str(topic.get("evidence_strength") or "unknown")
    return EvidenceFrame(
        object_id=str(topic.get("primary_object_id") or ""),
        object_type=str(topic.get("primary_object_type") or ""),
        ticker=str(topic.get("ticker") or ""),
        topic_id=str(topic.get("topic_id") or "") or None,
        anchor_terms=frozenset(facets["anchor_terms"]),
        predicate_terms=frozenset(facets["predicate_terms"]),
        context_facets=frozenset(facets["context_facets"]),
        core_domain_terms=frozenset(facets["core_domain_terms"]),
        sector_terms=frozenset(facets["sector_terms"]),
        mechanism_terms=frozenset(facets["mechanism_terms"]),
        impact_channels=frozenset(facets["impact_channels"]),
        generic_terms=frozenset(facets["generic_terms"]),
        evidence_strength=evidence_strength,
        support_quote_count=_safe_int(topic.get("support_quote_count")),
        support_claim_count=_safe_int(topic.get("support_claim_count")),
        specificity_score=_safe_float(topic.get("specificity_score"), default=0.5),
        materiality_hint=str(topic.get("materiality_hint") or "") or None,
    )


def classify_topic_match(query: QueryFrame, evidence: EvidenceFrame) -> dict[str, Any]:
    query_core_expanded = _expand_core_terms(query.core_domain_terms)
    evidence_core_expanded = _expand_core_terms(evidence.core_domain_terms)

    evidence_required_pool = (
        set(evidence.anchor_terms)
        | evidence_core_expanded
        | set(evidence.mechanism_terms)
        | set(evidence.sector_terms)
        | set(evidence.predicate_terms)
    )
    direct_check_requires_exact_anchors = bool(
        query.query_type == "direct_exposure_check" and query.must_for_direct
    )
    matched_required_facets = _matched_required(
        query.must_for_direct,
        evidence_required_pool,
        strict=direct_check_requires_exact_anchors,
        raw_evidence_terms=(
            set(evidence.anchor_terms)
            | set(evidence.core_domain_terms)
            | set(evidence.mechanism_terms)
            | set(evidence.sector_terms)
            | set(evidence.predicate_terms)
        ),
    )
    matched_core = query_core_expanded & evidence_core_expanded
    matched_mechanisms = set(query.mechanism_terms) & set(evidence.mechanism_terms)
    matched_sectors = set(query.sector_terms) & set(evidence.sector_terms)
    matched_channels = set(query.impact_channels) & set(evidence.impact_channels)
    matched_predicates = set(query.predicate_terms) & set(evidence.predicate_terms)
    matched_generic = set(query.generic_terms) & set(evidence.generic_terms)

    # A broad commodity premise can align with specific commodity evidence. Keep the
    # original specific evidence terms in the explanation where possible.
    explanation_core = sorted((set(evidence.core_domain_terms) & matched_core) or matched_core)

    direct_evidence = evidence.evidence_strength in {"direct", "strong"} or evidence.support_claim_count > 0 or evidence.support_quote_count > 0
    generic_only = not matched_required_facets and not matched_core and not matched_mechanisms and not matched_sectors and bool(matched_channels or matched_predicates or matched_generic)

    missing_required = _missing_required(
        query.must_for_direct,
        evidence.anchor_terms,
        evidence.core_domain_terms,
        evidence.mechanism_terms,
        evidence.sector_terms,
        evidence.predicate_terms,
        strict=direct_check_requires_exact_anchors,
    )
    anchor_score = _ratio_score(matched_required_facets, query.must_for_direct, default=1.0)
    predicate_score = _ratio_score(matched_predicates, query.predicate_terms, default=0.0)
    channel_score = _ratio_score(matched_channels, query.impact_channels, default=0.0)
    specificity_score = max(0.0, min(1.0, evidence.specificity_score))
    evidence_score = 1.0 if direct_evidence else 0.0
    generic_penalty = 0.25 if generic_only else 0.0
    missing_required_penalty = 0.35 if query.must_for_direct and anchor_score == 0 else min(0.25, 0.05 * len(missing_required))
    directness_score = (
        0.35 * anchor_score
        + 0.20 * predicate_score
        + 0.15 * channel_score
        + 0.20 * evidence_score
        + 0.10 * specificity_score
        - generic_penalty
        - missing_required_penalty
    )
    directness_score = max(0.0, min(1.0, directness_score))

    anchor_requirement_met = not query.must_for_direct or anchor_score > 0
    has_semantic_signal = bool(
        matched_required_facets
        or matched_core
        or matched_mechanisms
        or matched_sectors
        or matched_predicates
        or matched_channels
    )
    direct_required_complete = not (
        query.query_type == "direct_exposure_check"
        and query.must_for_direct
        and missing_required
    )
    if (
        anchor_requirement_met
        and direct_required_complete
        and direct_evidence
        and not generic_only
        and has_semantic_signal
        and directness_score >= 0.45
    ):
        tier = "direct"
    elif matched_required_facets or matched_core or matched_mechanisms or matched_sectors:
        tier = "strong_related"
    elif matched_channels or (
        matched_predicates and (matched_required_facets or matched_core or matched_mechanisms or matched_sectors)
    ):
        if query.query_type in {"direct_exposure_check", "scenario"} and query.must_for_direct and anchor_score == 0:
            tier = "insufficient"
        else:
            tier = "related"
    elif matched_generic:
        tier = "indirect"
    else:
        tier = "insufficient"

    core_rank_score = min(len(matched_core), 3)
    mechanism_rank_score = min(len(matched_mechanisms), 2)
    sector_rank_score = min(len(matched_sectors), 2)
    channel_rank_score = min(len(matched_channels), 3)
    generic_rank_penalty = 1.0 if generic_only else 0.0
    score = (
        4.0 * core_rank_score
        + 2.0 * mechanism_rank_score
        + 1.5 * sector_rank_score
        + 1.0 * channel_rank_score
        + 2.0 * evidence_score
        + evidence.specificity_score
        - 2.0 * generic_rank_penalty
    )
    normalized_score = max(0.0, min(1.0, score / 12.0))

    result: dict[str, Any] = {
        "tier": tier,
        "tier_rank": _TIER_RANK[tier],
        "score": round(normalized_score, 4),
        "directness_score": round(directness_score, 4),
        "anchor_score": round(anchor_score, 4),
        "predicate_score": round(predicate_score, 4),
        "channel_score": round(channel_score, 4),
        "specificity_score": round(specificity_score, 4),
        "generic_penalty": round(generic_penalty, 4),
        "matched_required_facets": sorted(matched_required_facets),
        "matched_core_terms": explanation_core,
        "matched_mechanisms": sorted(matched_mechanisms),
        "matched_sectors": sorted(matched_sectors),
        "matched_impact_channels": sorted(matched_channels),
        "matched_channel_facets": sorted(matched_channels),
        "matched_predicates": sorted(matched_predicates),
        "matched_generic_terms": sorted(matched_generic),
        "missing_required_facets": sorted(missing_required),
        "generic_only": generic_only,
        "evidence_strength": evidence.evidence_strength,
        "query_type": query.query_type,
    }
    if tier == "direct":
        result["why_direct"] = "Matched required anchor/core facet and soft predicate/channel signals with filing-backed evidence."
    else:
        missing = ", ".join(sorted(missing_required)) or "query anchor/core premise"
        result["why_not_direct"] = f"No filing-backed match for required core facet(s): {missing}."
    return result


def build_company_topic_profile(row: Mapping[str, Any], obj: Mapping[str, Any], retrieval_text: str) -> dict[str, Any] | None:
    object_type = str(obj.get("type") or row.get("type") or "")
    if object_type not in COMPANY_TOPIC_OBJECT_TYPES:
        return None
    object_id = str(obj.get("id") or row.get("id") or "")
    ticker = str(obj.get("ticker") or row.get("ticker") or "")
    if not object_id or not ticker:
        return None

    label = _display_topic_label(_topic_label(obj, object_type, object_id))
    topic_text = _compact_space(" ".join([label, _object_values_text(obj), retrieval_text]))[:12000]
    facets = _extract_facets(topic_text, is_query=False)
    impact_channels = sorted(facets["impact_channels"])
    factor_terms = sorted(facets["core_domain_terms"] | facets["sector_terms"] | facets["anchor_terms"])
    metric_terms = sorted(facets["impact_channels"])
    entity_terms = sorted(facets["anchor_terms"] | facets["context_facets"])
    mechanism_terms = sorted(facets["mechanism_terms"])
    scenario_terms = sorted(facets["predicate_terms"])
    source_ids = _source_object_ids(obj, object_id)
    support_quote_count = _support_count(obj, ("supported_by_quotes", "supporting_quotes", "evidence_quotes", "quote_ids"))
    support_claim_count = _support_count(obj, ("supported_by_claims", "supporting_claims", "research_claim_ids", "claim_ids"))
    support_metric_count = _support_count(obj, ("supported_by_metrics", "metric_ids", "related_metric_ids", "metric_observation_ids"))
    if object_type == "EvidenceQuote":
        support_quote_count = max(support_quote_count, 1)
    if object_type == "ResearchClaim":
        support_claim_count = max(support_claim_count, 1)

    evidence_strength = _evidence_strength(obj, object_type, support_quote_count, support_claim_count)
    specificity_score = _specificity_score(obj, facets)
    all_specific_terms = (
        set(factor_terms)
        | set(metric_terms)
        | set(entity_terms)
        | set(mechanism_terms)
        | set(scenario_terms)
    )
    generic_score = min(1.0, len(facets["generic_terms"]) / max(1, len(all_specific_terms)))
    label_normalized = _normalize(label).replace(" ", "_")
    boilerplate_score = 1.0 if label_normalized in {
        "business_activity",
        "project",
        "product_or_service_sales",
        "product_sales",
        "service_sales",
        "revenue_source",
        "risk",
        "external_factor",
        "metric",
        "operation",
    } else 0.0
    if boilerplate_score and specificity_score < 0.5:
        generic_score = max(generic_score, 0.8)
    topic_summary = _topic_summary(obj, topic_text)
    facet_text = " ".join(
        sorted(
            set(facets["core_domain_terms"])
            | set(facets["sector_terms"])
            | set(facets["mechanism_terms"])
            | set(facets["impact_channels"])
            | set(facets["anchor_terms"])
            | set(facets["predicate_terms"])
            | set(facets["context_facets"])
            | set(facets["generic_terms"])
        )
    )

    return {
        "topic_id": f"topic:{object_id}",
        "ticker": ticker,
        "period": row.get("period") or obj.get("period"),
        "document_type": row.get("document_type") or obj.get("document_type"),
        "doc_type_key": row.get("doc_type_key") or obj.get("doc_type_key"),
        "topic_label": label,
        "topic_summary": topic_summary,
        "topic_type": _topic_type(object_type),
        "topic_family": next(iter(sorted(facets["core_domain_terms"] or facets["sector_terms"] or facets["generic_terms"])), object_type),
        "topic_text": topic_text,
        "facet_text": facet_text,
        "primary_object_id": object_id,
        "primary_object_type": object_type,
        "source_object_ids": source_ids,
        "dominant_object_types": [object_type],
        "impact_channels": impact_channels,
        "factor_terms": factor_terms,
        "metric_terms": metric_terms,
        "entity_terms": entity_terms,
        "mechanism_terms": mechanism_terms,
        "scenario_terms": scenario_terms,
        "evidence_strength": evidence_strength,
        "materiality_hint": _string_or_none(obj.get("materiality_hint") or obj.get("materiality") or obj.get("importance")),
        "materiality_score": _materiality_score(obj),
        "specificity_score": specificity_score,
        "generic_score": generic_score,
        "boilerplate_score": boilerplate_score,
        "support_quote_count": support_quote_count,
        "support_claim_count": support_claim_count,
        "support_metric_count": support_metric_count,
    }


def topic_from_row(row: Mapping[str, Any]) -> dict[str, Any]:
    topic = dict(row)
    for key in (
        "source_object_ids",
        "top_traceable_object_ids",
        "untraced_object_ids",
        "dominant_object_types",
        "impact_channels",
        "factor_terms",
        "metric_terms",
        "entity_terms",
        "mechanism_terms",
        "scenario_terms",
    ):
        value = topic.get(key)
        if isinstance(value, str):
            try:
                decoded = json.loads(value)
            except json.JSONDecodeError:
                decoded = []
            topic[key] = decoded if isinstance(decoded, list) else []
    return topic


def _topic_type(object_type: str) -> str:
    if object_type in {"ExternalFactorExposure", "BusinessFactor", "RiskFactor", "GrowthDriver"}:
        return "risk_exposure"
    if object_type in {"AgreementTerm"}:
        return "agreement"
    if object_type in {"BusinessEvent", "ChangeEvent", "TrendObservation"}:
        return "event_or_change"
    if object_type in {"MetricObservation", "Calculation"}:
        return "metric"
    if object_type in {"BusinessActivity", "CompanyBusinessProfile"}:
        return "business_model"
    return "topic"


def _display_topic_label(label: str) -> str:
    value = str(label or "").replace("_", " ").replace("-", " ")
    value = _compact_space(value)
    if not value:
        return label
    lower_words = {"and", "or", "of", "to", "for", "in", "on", "by"}
    acronym_words = {"lng", "gpu", "hbm", "spa", "doe", "ferc", "cp2", "ttf", "jkm"}
    words = []
    for idx, word in enumerate(value.split()):
        normalized = word.lower()
        if normalized in acronym_words:
            words.append(normalized.upper())
        elif idx and normalized in lower_words:
            words.append(normalized)
        else:
            words.append(word[:1].upper() + word[1:])
    return " ".join(words)


def _materiality_score(obj: Mapping[str, Any]) -> float | None:
    raw = str(obj.get("materiality_hint") or obj.get("materiality") or obj.get("importance") or "").lower()
    if not raw:
        return None
    if any(term in raw for term in ("high", "material", "significant", "primary", "major")):
        return 1.0
    if any(term in raw for term in ("medium", "moderate")):
        return 0.6
    if any(term in raw for term in ("low", "immaterial", "minor")):
        return 0.2
    return 0.4


def tier_rank(tier: str) -> int:
    return _TIER_RANK.get(tier, 0)


def _extract_facets(text: str, *, is_query: bool) -> dict[str, set[str]]:
    raw_text = str(text or "")
    normalized = _normalize(text)
    core = _matches(normalized, _CORE_DOMAIN_PATTERNS)
    mechanisms = _matches(normalized, _MECHANISM_PATTERNS)
    sectors = _matches(normalized, _SECTOR_PATTERNS)
    channels = _matches(normalized, _IMPACT_CHANNEL_PATTERNS)
    anchors = _matches(normalized, _ANCHOR_PATTERNS)
    if not core and not mechanisms:
        anchors |= _metric_dimension_anchor_terms(raw_text)
    predicates = _matches(normalized, _PREDICATE_PATTERNS)
    contexts = _matches(normalized, _CONTEXT_PATTERNS)
    generic = _matches(normalized, _GENERIC_PATTERNS)

    has_volatility = _has_any(normalized, ("volatility", "volatile"))
    has_commodity_context = bool(core & {"commodity_price", "lng_market_price", "natural_gas_price", "crude_oil_price", "refined_product_price"})
    if has_volatility and has_commodity_context:
        core.add("commodity_price_volatility")
        generic.discard("market_volatility")
    elif has_volatility:
        generic.add("market_volatility")

    if is_query and ("chokepoint" in normalized or "bottleneck" in normalized):
        mechanisms.add("physical_supply_chokepoint")
    return {
        "core_domain_terms": core,
        "sector_terms": sectors,
        "mechanism_terms": mechanisms,
        "impact_channels": channels,
        "anchor_terms": anchors,
        "predicate_terms": predicates,
        "context_facets": contexts,
        "generic_terms": generic,
    }


def _infer_query_frame_type(raw_query: str, normalized: str, facets: Mapping[str, set[str]]) -> str:
    raw_lower = str(raw_query or "").lower()
    if any(marker in raw_lower for marker in ("직접", "direct", "directly")):
        return "direct_exposure_check"
    if any(
        marker in raw_lower
        for marker in ("if ", "what happens", "scenario", "시나리오", "좋은가", "나쁜가", "내려가", "올라가", "상승", "하락")
    ):
        return "scenario"
    metric_dimension_anchors = _metric_dimension_anchor_terms(raw_query)
    if metric_dimension_anchors and not facets.get("core_domain_terms") and not facets.get("mechanism_terms") and (
        facets.get("impact_channels") or facets.get("predicate_terms") or _has_any(normalized, ("growth", "driver", "demand"))
    ):
        return "product_revenue_driver"
    if _has_any(raw_lower, ("key risks", "main risks", "major risks", "principal risks", "주요 리스크", "핵심 리스크")):
        return "risk_overview"
    return "evidence_search"


def _must_for_direct(
    *,
    query_type: str,
    anchor_terms: set[str],
    core_terms: set[str],
    mechanism_terms: set[str],
) -> set[str]:
    if query_type == "risk_overview":
        return set()
    if anchor_terms and query_type in {"direct_exposure_check", "scenario", "product_revenue_driver"}:
        return set(anchor_terms)
    if query_type in {"direct_exposure_check", "scenario"}:
        return set(core_terms) or set(mechanism_terms)
    return set(core_terms)


def _matched_required(
    required: frozenset[str],
    evidence_pool: set[str],
    *,
    strict: bool = False,
    raw_evidence_terms: set[str] | None = None,
) -> set[str]:
    if strict:
        raw_terms = raw_evidence_terms or evidence_pool
        return {term for term in required if term in raw_terms}
    return {term for term in required if _facet_matches(term, evidence_pool)}


def _ratio_score(matched: set[str], required: frozenset[str], *, default: float) -> float:
    if not required:
        return default
    return min(1.0, len(matched) / len(required))


def _matches(text: str, patterns: Mapping[str, tuple[str, ...]]) -> set[str]:
    found: set[str] = set()
    for term, phrases in patterns.items():
        if _has_any(text, phrases):
            found.add(term)
    return found


def _has_any(text: str, phrases: tuple[str, ...]) -> bool:
    return any(_contains_phrase(text, phrase) for phrase in phrases)


def _metric_dimension_anchor_terms(text: str) -> set[str]:
    """Extract product/segment-like metric anchors without company/product vocabulary."""
    raw_tokens = re.findall(r"[A-Za-z][A-Za-z0-9]*", str(text or ""))
    if not raw_tokens:
        return set()
    metric_markers = {
        "sales",
        "revenue",
        "revenues",
        "demand",
        "orders",
        "bookings",
        "margin",
        "margins",
        "income",
    }
    stopwords = {
        "a",
        "an",
        "and",
        "are",
        "as",
        "by",
        "company",
        "consolidated",
        "due",
        "for",
        "from",
        "growth",
        "higher",
        "increased",
        "lower",
        "net",
        "of",
        "operating",
        "revenue",
        "revenues",
        "sales",
        "strong",
        "the",
        "to",
        "total",
        "was",
        "were",
        "year",
    }
    anchors: set[str] = set()
    normalized_tokens = [token.lower() for token in raw_tokens]
    for idx, token in enumerate(normalized_tokens):
        if token not in metric_markers:
            continue
        candidate_parts: list[str] = []
        raw_candidate_parts: list[str] = []
        cursor = idx - 1
        skipped_stopword = False
        while cursor >= 0 and len(candidate_parts) < 3:
            normalized_part = normalized_tokens[cursor]
            raw_part = raw_tokens[cursor]
            if normalized_part in stopwords:
                skipped_stopword = True
                cursor -= 1
                continue
            if skipped_stopword and candidate_parts:
                break
            candidate_parts.insert(0, normalized_part)
            raw_candidate_parts.insert(0, raw_part)
            cursor -= 1
        if not candidate_parts:
            continue
        raw_candidate = "".join(raw_candidate_parts)
        if raw_candidate.isupper() and 1 <= len(raw_candidate) <= 5:
            continue
        key = re.sub(r"[^a-z0-9]+", "_", " ".join(candidate_parts)).strip("_")
        if key and key not in stopwords:
            anchors.add(key)
    return anchors


def _contains_phrase(text: str, phrase: str) -> bool:
    normalized_phrase = _normalize(phrase)
    if " " in normalized_phrase:
        return normalized_phrase in text
    return re.search(rf"(?<![a-z0-9]){re.escape(normalized_phrase)}(?![a-z0-9])", text) is not None


def _normalize(value: str) -> str:
    lowered = str(value or "").lower().replace("_", " ").replace("-", " ")
    lowered = re.sub(r"[^a-z0-9가-힣%.$]+", " ", lowered)
    return _compact_space(lowered)


def _compact_space(value: str) -> str:
    return re.sub(r"\s+", " ", value or "").strip()


def _expand_core_terms(terms: frozenset[str] | set[str]) -> set[str]:
    expanded = set(terms)
    frontier = list(terms)
    while frontier:
        term = frontier.pop()
        for parent in _CORE_PARENT_TERMS.get(term, set()):
            if parent not in expanded:
                expanded.add(parent)
                frontier.append(parent)
    return expanded


def _missing_required(
    required: frozenset[str],
    evidence_anchors: frozenset[str],
    evidence_core: frozenset[str],
    evidence_mechanisms: frozenset[str],
    evidence_sectors: frozenset[str],
    evidence_predicates: frozenset[str],
    *,
    strict: bool = False,
) -> set[str]:
    if not required:
        return set()
    if strict:
        evidence_terms = (
            set(evidence_anchors)
            | set(evidence_core)
            | set(evidence_mechanisms)
            | set(evidence_sectors)
            | set(evidence_predicates)
        )
        return {term for term in required if term not in evidence_terms}
    evidence_expanded = (
        set(evidence_anchors)
        | _expand_core_terms(evidence_core)
        | set(evidence_mechanisms)
        | set(evidence_sectors)
        | set(evidence_predicates)
    )
    missing = set()
    for term in required:
        if not _facet_matches(term, evidence_expanded):
            missing.add(term)
    return missing


def _facet_matches(term: str, evidence_terms: set[str]) -> bool:
    if term in evidence_terms:
        return True
    term_expanded = _expand_core_terms(frozenset({term}))
    if term_expanded & evidence_terms:
        return True
    equivalents = {
        "henry_hub": {"natural_gas_price", "feed_gas", "natural_gas", "feed_gas_procurement"},
        "feed_gas": {"natural_gas_price", "henry_hub", "natural_gas", "feed_gas_procurement"},
        "natural_gas": {"natural_gas_price", "henry_hub", "feed_gas", "feed_gas_procurement"},
        "lng": {"lng_market_price", "energy_lng", "lng_sales"},
        "spa": {"lng_sales", "take_or_pay"},
        "take_or_pay": {"lng_sales", "spa"},
        "debt_acceleration": {"capital_access"},
        "termination": {"contract_termination"},
    }.get(term, set())
    return bool(equivalents & evidence_terms)


def _premises_from_terms(core_terms: set[str], mechanism_terms: set[str]) -> list[dict[str, Any]]:
    premises: list[dict[str, Any]] = []
    labels = {
        "commodity_price_volatility": "commodity price volatility",
        "commodity_price": "commodity price exposure",
        "lng_market_price": "LNG or natural gas exposure",
        "natural_gas_price": "natural gas or feed gas exposure",
        "crude_oil_price": "crude oil price exposure",
        "shipping_chokepoint": "physical or shipping chokepoint",
    }
    for term in sorted(core_terms):
        premises.append({"name": labels.get(term, term.replace("_", " ")), "required_for_direct": True})
    for term in sorted(mechanism_terms):
        if term in {"shipping_constraint", "physical_supply_chokepoint"} and not any(p["name"] == "physical or shipping chokepoint" for p in premises):
            premises.append({"name": "physical or shipping chokepoint", "required_for_direct": False})
    return premises


def _topic_match_text(topic: Mapping[str, Any]) -> str:
    parts = [
        str(topic.get("topic_label") or ""),
        str(topic.get("topic_summary") or ""),
        str(topic.get("topic_text") or ""),
        str(topic.get("facet_text") or ""),
        " ".join(str(v) for v in topic.get("impact_channels") or []),
    ]
    return _normalize(" ".join(parts))


def _topic_label(obj: Mapping[str, Any], object_type: str, object_id: str) -> str:
    for key in ("topic_label", "label", "name", "factor", "risk_name", "driver", "activity", "event", "term", "metric", "claim_text", "quote_text", "summary", "description"):
        value = obj.get(key)
        if isinstance(value, str) and value.strip():
            return _compact_space(value)[:180]
    return f"{object_type} {object_id}"


def _topic_summary(obj: Mapping[str, Any], topic_text: str) -> str:
    for key in ("topic_summary", "summary", "description", "claim_text", "quote_text"):
        value = obj.get(key)
        if isinstance(value, str) and value.strip():
            return _compact_space(value)[:500]
    return topic_text[:500]


def _object_values_text(obj: Mapping[str, Any]) -> str:
    values: list[str] = []
    for key, value in obj.items():
        if key in {"id", "type"}:
            continue
        if isinstance(value, str):
            values.append(value)
        elif isinstance(value, (list, tuple)):
            values.extend(str(item) for item in value if isinstance(item, (str, int, float)))
        elif isinstance(value, dict):
            values.extend(str(item) for item in value.values() if isinstance(item, (str, int, float)))
    return _compact_space(" ".join(values))


def _source_object_ids(obj: Mapping[str, Any], object_id: str) -> list[str]:
    ids: list[str] = [object_id]
    for key in ("source_object_ids", "supported_by_claims", "supported_by_quotes", "supporting_claims", "supporting_quotes", "claim_ids", "quote_ids"):
        value = obj.get(key)
        if isinstance(value, str):
            if value.startswith(
                (
                    "claim:",
                    "research_claim:",
                    "quote:",
                    "evidence_quote:",
                    "risk:",
                    "exposure:",
                    "external_factor_exposure:",
                    "factor:",
                    "business_factor:",
                    "metric:",
                    "metric_observation:",
                    "calculation:",
                    "agreement:",
                    "agreement_term:",
                    "business_event:",
                    "event:",
                    "business_activity:",
                )
            ):
                ids.append(value)
        elif isinstance(value, list):
            ids.extend(str(item) for item in value if isinstance(item, str))
    deduped: list[str] = []
    seen: set[str] = set()
    for item in ids:
        if item and item not in seen:
            seen.add(item)
            deduped.append(item)
    return deduped[:20]


def _support_count(obj: Mapping[str, Any], keys: tuple[str, ...]) -> int:
    count = 0
    for key in keys:
        value = obj.get(key)
        if isinstance(value, list):
            count += len(value)
        elif isinstance(value, str) and value.strip():
            count += 1
    return count


def _evidence_strength(obj: Mapping[str, Any], object_type: str, quote_count: int, claim_count: int) -> str:
    explicit = str(obj.get("evidence_strength") or obj.get("evidence_grade") or "").lower()
    if explicit in {"direct", "strong", "related", "indirect"}:
        return explicit
    if object_type in _DIRECT_EVIDENCE_TYPES:
        return "direct"
    if quote_count or claim_count or object_type in _STRONG_EVIDENCE_TYPES:
        return "strong"
    return "unknown"


def _specificity_score(obj: Mapping[str, Any], facets: Mapping[str, set[str]]) -> float:
    explicit = _safe_float(obj.get("specificity_score"), default=-1.0)
    if explicit >= 0:
        return max(0.0, min(1.0, explicit))
    specific_count = len(facets.get("core_domain_terms", set())) + len(facets.get("mechanism_terms", set()))
    generic_count = len(facets.get("generic_terms", set()))
    score = 0.45 + 0.12 * specific_count - 0.05 * generic_count
    return max(0.1, min(1.0, score))


def _string_or_none(value: Any) -> str | None:
    if value is None:
        return None
    text = str(value).strip()
    return text or None


def _safe_int(value: Any) -> int:
    try:
        return int(value or 0)
    except (TypeError, ValueError):
        return 0


def _safe_float(value: Any, *, default: float) -> float:
    try:
        return float(value)
    except (TypeError, ValueError):
        return default
