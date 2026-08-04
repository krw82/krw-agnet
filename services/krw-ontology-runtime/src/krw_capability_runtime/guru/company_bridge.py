"""Serving-layer bridge between guru lenses and company filing research.

This module does not import or mutate the KRW company ontology. It only turns
Guru ResearchPack evidence needs into bounded payloads that an application
orchestrator can pass to the existing company filing MCP.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping, Sequence
import hashlib
import json
import re
from typing import Any

from pydantic import ValidationError

from krw_capability_runtime.guru.context_taxonomy import (
    context_tags_for_text,
    filing_topic_filter_reason,
)
from krw_capability_runtime.guru.company_context import (
    coerce_company_context,
    company_context_topics,
)
from krw_capability_runtime.guru.models import (
    AUTHOR_KEYS,
    GURU_INVESTIGATION_BRIEF_FORMAT,
    GuruAgentEvidenceAnalysis,
    GuruDynamicQuestionPlan,
    GuruDynamicQuestionPlanItem,
    GuruCompanyEvidenceAlignment,
    GuruCompanyEvidenceReview,
    GuruCompanyFilingBrief,
    GuruCompanyIdentity,
    GuruCompanyOntologyContext,
    GuruCompanyResearchContext,
    GuruCompanyResearchPack,
    GuruInvestigationBrief,
    GuruInvestigationQuestion,
    GuruInvestigationQuestionDraft,
    GuruLightCompanyContext,
    GuruValidatedEvidenceAnalysis,
)


class GuruEvidenceAnalysisValidationError(ValueError):
    """A precise, model-actionable correction for one Guru review call."""

    def __init__(
        self,
        *,
        code: str,
        message: str,
        required_change: str,
        invalid_fields: list[str],
        violations: list[dict[str, Any]] | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.required_change = required_change
        self.invalid_fields = invalid_fields
        self.violations = violations or []


TOPIC_QUERY_EXPANSIONS: dict[str, tuple[str, ...]] = {
    "business_model": (
        "business model",
        "segments",
        "revenue drivers",
        "competitive position",
        "사업 모델",
    ),
    "cash_flow": (
        "operating cash flow",
        "free cash flow",
        "capital expenditures",
        "working capital",
        "현금흐름",
    ),
    "cash_generation": (
        "cash generation",
        "operating cash flow",
        "free cash flow",
        "cash conversion",
        "현금창출",
    ),
    "capital_allocation": (
        "capital allocation",
        "reinvestment",
        "acquisitions",
        "dividends",
        "share repurchases",
        "자본배분",
    ),
    "share_repurchases": ("share repurchases", "buybacks", "treasury stock", "자사주"),
    "dividends": ("dividends", "payout", "배당"),
    "reinvestment": ("reinvestment", "capex", "growth investment", "재투자"),
    "balance_sheet": (
        "debt",
        "liquidity",
        "debt maturity",
        "interest expense",
        "leverage",
        "대차대조표",
    ),
    "risk_factors": (
        "risk factors",
        "regulatory risk",
        "commodity price risk",
        "competition",
        "리스크 요인",
    ),
    "valuation_context": (
        "valuation drivers",
        "earnings power",
        "margin durability",
        "growth duration",
        "밸류에이션",
    ),
    "margin_structure": ("gross margin", "operating margin", "cost structure", "마진"),
    "growth_duration": ("growth durability", "long-term growth", "market demand", "성장 지속성"),
    "demand_cycle_exposure": ("demand cycle", "cyclical demand", "end-market demand", "수요 사이클"),
    "margin_pressure": ("margin pressure", "pricing", "input costs", "cost inflation", "마진 압박"),
    "liquidity": ("liquidity", "cash and equivalents", "credit facility", "유동성"),
    "capital_intensity": ("capital intensity", "capex", "maintenance capital", "자본집약도"),
    "services_mix": ("services mix", "segment mix", "recurring revenue", "서비스 믹스"),
    "customer_demand": ("customer demand", "unit sales", "retention", "고객 수요"),
    "commodity_price_exposure": (
        "commodity price exposure",
        "oil price",
        "gas price",
        "hedging",
        "원자재 가격",
    ),
}

TOPIC_KO_LABELS: dict[str, str] = {
    "business_model": "사업 모델",
    "cash_flow": "영업현금흐름과 자유현금흐름",
    "cash_generation": "현금창출력",
    "capital_allocation": "자본배분",
    "share_repurchases": "자사주",
    "dividends": "배당",
    "reinvestment": "재투자",
    "balance_sheet": "부채와 유동성",
    "risk_factors": "리스크 요인",
    "valuation_context": "가격 판단에 필요한 사업 근거",
    "margin_structure": "마진 구조",
    "growth_duration": "성장 지속성",
    "demand_cycle_exposure": "수요 사이클 노출",
    "margin_pressure": "마진 압박",
    "liquidity": "유동성",
    "capital_intensity": "자본집약도",
    "services_mix": "서비스 믹스",
    "customer_demand": "고객 수요",
    "commodity_price_exposure": "원자재 가격 민감도",
}

AUTHOR_DISPLAY_NAME_FOR_BRIDGE: dict[str, str] = {
    "buffett": "Buffett",
    "marks": "Marks",
    "ackman": "Ackman",
    "flatt": "Flatt",
    "terry_smith": "Terry Smith",
}

TOPIC_QUESTION_TEMPLATES: dict[str, tuple[str, str, str]] = {
    "business_model": (
        "For {subject}, what actually drives the economics, and does that business model deserve the selected guru's patience?",
        "사업 모델이 정말 오래 갈 구조인가?",
        "Do not treat a market story as business quality unless filings show the economics of the core business.",
    ),
    "cash_flow": (
        "For {subject}, does filing evidence show durable cash generation after reinvestment and working-capital pressure?",
        "현금창출력이 오래 버틸 구조인가?",
        "Do not call cash generation durable if it depends on one strong cycle or temporary working-capital help.",
    ),
    "cash_generation": (
        "For {subject}, is cash generation a repeatable owner-earnings engine or a period-specific outcome?",
        "현금창출이 반복 가능한가?",
        "Do not convert one period of strong cash flow into a durable quality claim.",
    ),
    "capital_allocation": (
        "For {subject}, does management allocate capital with discipline, or is it buying growth and stock at the wrong time?",
        "자본배분이 가격과 사이클에 민감했나?",
        "Do not praise capital allocation unless filings show discipline across reinvestment, acquisitions, dividends, and repurchases.",
    ),
    "share_repurchases": (
        "For {subject}, do share repurchases appear price-sensitive and value-accretive, or merely a use of excess cash?",
        "자사주 매입에 가격 규율이 있었나?",
        "Do not treat every buyback as shareholder-friendly without price discipline or capital priority evidence.",
    ),
    "dividends": (
        "For {subject}, is the dividend supported by recurring cash generation rather than balance-sheet strain?",
        "배당은 현금흐름으로 지탱되나?",
        "Do not call a dividend safe without cash-flow and reinvestment context.",
    ),
    "balance_sheet": (
        "For {subject}, would the balance sheet let the investor wait through an adverse cycle?",
        "불리한 사이클을 버틸 재무 구조인가?",
        "Do not downplay leverage or liquidity pressure when the thesis needs time.",
    ),
    "risk_factors": (
        "For {subject}, which disclosed risks would most directly break the guru-style thesis?",
        "어떤 리스크가 투자 가정을 깨나?",
        "Do not list generic risks; isolate the risk that would change the investor's judgment.",
    ),
    "valuation_context": (
        "For {subject}, what filing-supported business facts matter before discussing whether the price is sensible?",
        "가격 판단 전에 어떤 사업 근거가 필요한가?",
        "Do not imply fair value or target price from filing facts alone.",
    ),
    "margin_structure": (
        "For {subject}, are margins protected by business quality, or vulnerable to mix, input cost, pricing, and competition?",
        "마진은 보호되는가, 압박받는가?",
        "Do not treat high margins as durable without evidence of why they persist.",
    ),
    "growth_duration": (
        "For {subject}, is growth likely to persist because of business quality, or is it front-loaded and vulnerable to normalization?",
        "성장이 오래 지속될 근거가 있나?",
        "Do not treat recent growth as duration unless filings show repeatable demand and economics.",
    ),
    "demand_cycle_exposure": (
        "For {subject}, how much of the thesis still depends on the customer demand or capex cycle?",
        "수요·투자 사이클 의존이 얼마나 큰가?",
        "Do not frame cyclicality as reduced unless filings show the core economics are less cycle-dependent.",
    ),
    "margin_pressure": (
        "For {subject}, what disclosed cost, pricing, or mix pressure could compress future margins?",
        "마진 압박은 어디서 오나?",
        "Do not offset margin pressure with growth language unless the evidence directly connects them.",
    ),
    "liquidity": (
        "For {subject}, is liquidity enough to avoid forced decisions if conditions worsen?",
        "상황 악화 때 강제 선택을 피할 유동성이 있나?",
        "Do not confuse available liquidity with long-term business quality.",
    ),
    "capital_intensity": (
        "For {subject}, does the business need heavy reinvestment just to stand still?",
        "유지에 돈이 많이 드는 사업인가?",
        "Do not ignore maintenance capital when judging cash generation.",
    ),
    "services_mix": (
        "For {subject}, does the service or recurring mix materially improve durability, or remain dependent on the core product cycle?",
        "서비스·반복 매출이 내구성을 바꾸나?",
        "Do not call the business recurring if the recurring layer still rides on the core product base.",
    ),
    "customer_demand": (
        "For {subject}, does customer demand look durable, concentrated, or cyclical in the filings?",
        "고객 수요는 내구적인가, 순환적인가?",
        "Do not treat demand as durable without concentration and cycle context.",
    ),
    "commodity_price_exposure": (
        "For {subject}, how much of the investment case still depends on commodity prices rather than controllable business quality?",
        "원자재 가격 의존이 얼마나 큰가?",
        "Do not make commodity exposure look like a durable moat unless filings show controllable advantages.",
    ),
}

def company_filing_brief_from_guru_lens(
    guru_payload: Mapping[str, Any],
    *,
    ticker: str | None = None,
    company_name: str | None = None,
    company_context: GuruCompanyOntologyContext | Mapping[str, Any] | None = None,
    author_key: str | None = None,
    question: str | None = None,
    investigation_questions: Sequence[Mapping[str, Any]] | None = None,
) -> GuruCompanyFilingBrief:
    """Translate a Guru ResearchPack or query-context payload into a filing brief."""

    payload = _mapping(guru_payload)
    pack = _research_pack(payload)
    selected_author = _author_key(author_key, pack)
    original_question = (
        _clean_optional(question)
        or _clean_optional(_get_nested(pack, ("pack_meta", "question")))
        or _clean_optional(payload.get("question"))
        or ""
    )
    bridge = _mapping(pack.get("company_bridge") or payload.get("company_bridge"))
    resolved_ticker = _clean_optional(ticker) or _clean_optional(bridge.get("ticker"))
    resolved_company_name = _clean_optional(company_name)
    subject = _company_subject(resolved_ticker, resolved_company_name)
    context_model = coerce_company_context(
        company_context or pack.get("company_context") or payload.get("company_context") or None,
        ticker=resolved_ticker,
        company_name=resolved_company_name,
    )
    requires_company = bool(
        bridge.get("requires_company_evidence")
        or payload.get("requires_company_evidence")
        or resolved_ticker
        or resolved_company_name
    )
    requires_identifier = bool(bridge.get("requires_identifier_clarification"))
    generic_requirements = _unique_strings(
        bridge.get("filing_evidence_requirements")
        or payload.get("filing_evidence_requirements")
        or []
    )
    conditional_requirements = _unique_strings(
        bridge.get("conditional_filing_evidence_requirements") or []
    )
    if not generic_requirements and requires_company:
        generic_requirements = conditional_requirements
    lens_specific = _lens_specific_evidence_requests(pack)
    context_topics = company_context_topics(context_model)
    candidate_topics = _unique_strings([*context_topics, *generic_requirements, *lens_specific])
    if "energy" in _context_tags(original_question, subject, context_model):
        candidate_topics = _unique_strings([*candidate_topics, "commodity_price_exposure"])
    topic_filter = _filter_filing_topics(
        topics=candidate_topics,
        question=original_question,
        subject=subject,
        company_context=context_model,
        generic_requirements=generic_requirements,
    )
    required_topics = topic_filter["required"]
    filtered_topics = topic_filter["filtered"]
    data_need_keys = _data_need_keys(pack)
    query_data_need_keys = _filter_query_data_need_keys(
        data_need_keys,
        question=original_question,
        subject=subject,
        company_context=context_model,
    )
    query_terms = _query_terms(
        subject=subject,
        ticker=resolved_ticker,
        company_name=resolved_company_name,
        topics=required_topics,
        data_need_keys=query_data_need_keys,
    )
    missing_inputs = _missing_inputs(
        requires_company=requires_company,
        requires_identifier=requires_identifier,
        ticker=resolved_ticker,
        company_name=resolved_company_name,
    )
    question_ko = _company_question_ko(
        subject=subject,
        original_question=original_question,
        topics=required_topics or conditional_requirements,
    )
    question_en = _company_question_en(
        subject=subject,
        original_question=original_question,
        topics=required_topics or conditional_requirements,
        query_terms=query_terms,
    )
    recommended_call = {
        "tool": "krw_ontology_query_context",
        "when": (
            "after_identifier_resolution"
            if requires_identifier
            else "now"
            if requires_company and not missing_inputs
            else "only_after_company_or_ticker_is_selected"
        ),
        "arguments": {
            "question": question_ko,
            "ticker": resolved_ticker,
        },
        "policy": "Application/orchestrator calls the company MCP; Guru MCP remains read-only lens context.",
    }
    investigation_brief = (
        seal_guru_investigation_brief(
            guru_payload=payload,
            investigation_questions=investigation_questions,
            ticker=resolved_ticker,
            company_context=company_context,
            author_key=selected_author,
        )
        if investigation_questions is not None
        else None
    )
    dynamic_question_plan = (
        None
        if investigation_questions is not None
        else _dynamic_question_plan(
            author_key=selected_author,
            subject=subject,
            ticker=resolved_ticker,
            company_name=resolved_company_name,
            original_question=original_question,
            pack=pack,
            company_context=context_model,
            required_topics=required_topics if requires_company else [],
            candidate_topics=candidate_topics if requires_company else [],
            data_need_keys=data_need_keys,
            query_terms=query_terms,
            intent_family=_clean_optional(_get_nested(pack, ("intent", "family"))),
        )
    )
    return GuruCompanyFilingBrief(
        author_key=selected_author,
        original_question=original_question,
        company_identity=GuruCompanyIdentity(
            ticker=resolved_ticker,
            company_name=resolved_company_name,
            subject=subject,
            unresolved=requires_identifier or bool(missing_inputs),
        ),
        requires_company_evidence=requires_company,
        requires_identifier_clarification=requires_identifier,
        company_context=(
            context_model.model_dump(mode="json", exclude_none=True)
            if context_model is not None
            else {}
        ),
        company_research_question_ko=question_ko,
        company_research_question_en=question_en,
        query_terms=query_terms,
        required_filing_topics=required_topics if requires_company else [],
        candidate_filing_topics=candidate_topics if requires_company else [],
        filtered_out_topics=filtered_topics if requires_company else [],
        lens_specific_evidence_requests=lens_specific,
        generic_filing_requirements=generic_requirements,
        data_need_keys=data_need_keys,
        dynamic_question_plan=dynamic_question_plan if requires_company else None,
        investigation_brief=investigation_brief if requires_company else None,
        missing_inputs=missing_inputs,
        recommended_company_mcp_call=recommended_call,
    )


def seal_guru_investigation_brief(
    *,
    guru_payload: Mapping[str, Any],
    investigation_questions: Sequence[Mapping[str, Any]],
    ticker: str | None,
    company_context: GuruCompanyOntologyContext | Mapping[str, Any] | None,
    author_key: str | None = None,
) -> GuruInvestigationBrief:
    """Validate and seal one main-agent key question without authoring it.

    The server owns identity, source binding, deterministic ids, and hashes.
    The main Guru owns its central question selection and later interpretation.
    """

    payload = _mapping(guru_payload)
    pack = _research_pack(payload)
    selected_author = _author_key(author_key, pack)
    research_pack_id = _clean_optional(_get_nested(pack, ("pack_meta", "pack_id")))
    normalized_ticker = _clean_optional(ticker)
    if not research_pack_id:
        raise ValueError("Guru ResearchPack must contain pack_meta.pack_id")
    if not normalized_ticker:
        raise ValueError("ticker is required to seal a Guru investigation brief")
    if len(investigation_questions) != 1:
        raise ValueError("investigation_questions must contain exactly one key question")

    context = _coerce_light_company_context(
        company_context,
        ticker=normalized_ticker,
    )
    if context.ticker.upper() != normalized_ticker.upper():
        raise ValueError("company_context ticker does not match the requested ticker")
    context_anchor_ids = {anchor.anchor_id for anchor in context.context_anchors}
    if not context_anchor_ids:
        raise ValueError("company_context must include trusted context_anchors")

    selected_lenses = _list_of_mappings(pack.get("selected_lenses"))
    allowed_principle_ids = {
        str(lens.get("reviewed_id"))
        for lens in selected_lenses
        if lens.get("reviewed_id") and lens.get("author_key", selected_author) == selected_author
    }
    if not allowed_principle_ids:
        raise ValueError("Guru ResearchPack contains no selectable principle ids for the author")

    normalized_drafts = [
        _normalize_investigation_draft(raw_draft)
        for raw_draft in investigation_questions
    ]
    if not normalized_drafts[0].get("decision_role"):
        # The one-question investigation is necessarily the central tension.
        # This is a structural default, not a server-authored investment question.
        normalized_drafts[0]["decision_role"] = "main_tension"
    if normalized_drafts[0].get("decision_role") != "main_tension":
        raise ValueError(
            "the sole investigation question must declare decision_role=main_tension"
        )

    sealed_questions: list[GuruInvestigationQuestion] = []
    normalized_question_texts: set[str] = set()
    for raw_draft in normalized_drafts:
        draft = GuruInvestigationQuestionDraft.model_validate(
            raw_draft
        )
        _require_investigation_draft_fields(draft)
        unknown_principles = set(draft.guru_principle_ids) - allowed_principle_ids
        if unknown_principles:
            raise ValueError(
                "investigation question references a principle outside the selected "
                f"Guru ResearchPack: {sorted(unknown_principles)}"
            )
        unknown_anchors = set(draft.company_context_anchor_ids) - context_anchor_ids
        if unknown_anchors:
            raise ValueError(
                "investigation question references an unknown company context anchor: "
                f"{sorted(unknown_anchors)}"
            )
        normalized_question = _normalize_question_text(draft.question)
        if normalized_question in normalized_question_texts:
            raise ValueError("investigation_questions must not contain duplicate questions")
        normalized_question_texts.add(normalized_question)
        question_id = _sealed_question_id(
            author_key=selected_author,
            ticker=normalized_ticker,
            question=draft,
        )
        sealed_questions.append(
            GuruInvestigationQuestion(
                **draft.model_dump(mode="python"),
                question_id=question_id,
            )
        )

    context_hash = _canonical_hash(context.model_dump(mode="json"))
    unsigned = {
        "format": GURU_INVESTIGATION_BRIEF_FORMAT,
        "research_pack_id": research_pack_id,
        "author_key": selected_author,
        "ticker": normalized_ticker.upper(),
        "company_context_hash": context_hash,
        "questions": [question.model_dump(mode="json") for question in sealed_questions],
    }
    return GuruInvestigationBrief(
        **unsigned,
        brief_hash=_canonical_hash(unsigned),
    )


def validate_guru_agent_evidence_analysis(
    *,
    investigation_brief: GuruInvestigationBrief | Mapping[str, Any],
    company_research_context: GuruCompanyResearchContext | Mapping[str, Any],
    agent_analysis: GuruAgentEvidenceAnalysis | Mapping[str, Any],
) -> GuruValidatedEvidenceAnalysis:
    """Validate that a Guru analysis stays inside sealed research-context bounds."""

    brief = (
        investigation_brief
        if isinstance(investigation_brief, GuruInvestigationBrief)
        else GuruInvestigationBrief.model_validate(_normalize_investigation_brief(investigation_brief))
    )
    try:
        research_context = (
            company_research_context
            if isinstance(company_research_context, GuruCompanyResearchContext)
            else GuruCompanyResearchContext.model_validate(dict(company_research_context))
        )
    except ValidationError as exc:
        raise GuruEvidenceAnalysisValidationError(
            code="invalid_company_research_context",
            message="company_research_context is not a valid runtime-built context.",
            required_change=(
                "Attach the exact krw-guru-company-research-context/v1 payload "
                "created from this run's filing-tool results. Do not create, edit, "
                "or omit its format, ticker, brief_hash, question_ids, evidence_units, "
                "or source_object_ids."
            ),
            invalid_fields=["company_research_context"],
            violations=[{"validation_errors": exc.errors()}],
        ) from exc
    if research_context.brief_hash != brief.brief_hash:
        raise ValueError("company research context brief_hash does not match the sealed investigation brief")
    if research_context.ticker.upper() != brief.ticker.upper():
        raise ValueError("company research context ticker does not match the sealed investigation brief")

    analysis = (
        agent_analysis
        if isinstance(agent_analysis, GuruAgentEvidenceAnalysis)
        else GuruAgentEvidenceAnalysis.model_validate(
            _normalize_agent_evidence_analysis(agent_analysis)
        )
    )
    questions_by_id = {question.question_id: question for question in brief.questions}
    if set(research_context.question_ids) != set(questions_by_id):
        raise GuruEvidenceAnalysisValidationError(
            code="sealed_question_context_required",
            message="company_research_context must cover the sealed investigation question exactly.",
            required_change=(
                "Attach the runtime-built company_research_context for the current "
                "sealed brief without adding or removing question_ids."
            ),
            invalid_fields=["company_research_context.question_ids"],
            violations=[
                {
                    "allowed_question_ids": sorted(questions_by_id),
                    "received_question_ids": sorted(research_context.question_ids),
                }
            ],
        )
    permitted_ids = set(research_context.source_object_ids)
    assessment_ids = [assessment.question_id for assessment in analysis.assessments]
    if set(assessment_ids) != set(questions_by_id) or len(assessment_ids) != len(set(assessment_ids)):
        missing = sorted(set(questions_by_id) - set(assessment_ids))
        unexpected = sorted(set(assessment_ids) - set(questions_by_id))
        raise GuruEvidenceAnalysisValidationError(
            code="sealed_question_coverage_required",
            message="agent_analysis must assess every sealed investigation question exactly once.",
            required_change=(
                "Use exactly these sealed question_id values once each: "
                f"{sorted(questions_by_id)}."
            ),
            invalid_fields=["agent_analysis.assessments"],
            violations=[
                {
                    "missing_question_ids": missing,
                    "unexpected_question_ids": unexpected,
                    "allowed_question_ids": sorted(questions_by_id),
                }
            ],
        )

    for assessment_index, assessment in enumerate(analysis.assessments):
        question = questions_by_id[assessment.question_id]
        _require_nonempty_text(assessment.reasoning, "assessment.reasoning")
        if not set(assessment.evidence_object_ids).issubset(permitted_ids):
            raise GuruEvidenceAnalysisValidationError(
                code="question_scoped_evidence_required",
                message=(
                    f"Question {assessment.question_id} cites evidence outside its "
                    "runtime-built company research context."
                ),
                required_change=(
                    "Use only the allowed evidence_object_ids for this question, or "
                    "remove the citation and choose mixed or unresolved."
                ),
                invalid_fields=[
                    f"agent_analysis.assessments[{assessment_index}].evidence_object_ids"
                ],
                violations=[
                    {
                        "question_id": assessment.question_id,
                        "allowed_evidence_object_ids": sorted(permitted_ids),
                    }
                ],
            )

        allowed_verdicts = ["mixed", "unresolved"]
        if assessment.verdict not in allowed_verdicts:
            raise GuruEvidenceAnalysisValidationError(
                code="contextual_evidence_cannot_support",
                message=(
                    f"Question {assessment.question_id} cannot use verdict "
                    f"'{assessment.verdict}' with contextual company research."
                ),
                required_change=(
                    "Choose mixed or unresolved. Contextual research may inform an "
                    "interpretation but cannot establish a supported verdict."
                ),
                invalid_fields=[
                    f"agent_analysis.assessments[{assessment_index}].verdict"
                ],
                violations=[
                    {
                        "question_id": assessment.question_id,
                        "allowed_verdicts": allowed_verdicts,
                        "allowed_evidence_object_ids": sorted(permitted_ids),
                    }
                ],
            )
        if assessment.verdict == "unresolved" and assessment.evidence_object_ids:
            raise GuruEvidenceAnalysisValidationError(
                code="unresolved_verdict_must_not_cite_evidence",
                message=(
                    f"Question {assessment.question_id} uses verdict 'unresolved' but "
                    "also cites evidence."
                ),
                required_change=(
                    "Remove evidence_object_ids for an unresolved verdict, or change "
                    "the verdict to mixed."
                ),
                invalid_fields=[
                    f"agent_analysis.assessments[{assessment_index}].evidence_object_ids",
                    f"agent_analysis.assessments[{assessment_index}].verdict",
                ],
                violations=[
                    {
                        "question_id": assessment.question_id,
                        "allowed_verdicts": allowed_verdicts,
                        "allowed_evidence_object_ids": sorted(permitted_ids),
                    }
                ],
            )

    _require_nonempty_text(analysis.overall_judgment, "overall_judgment")
    decision_frame = {
        "main_tension_question_id": next(
            question.question_id
            for question in brief.questions
            if question.decision_role == "main_tension"
        ),
        "questions": [
            {
                "question_id": question.question_id,
                "decision_role": question.decision_role,
                "question": question.question,
                "hypothesis": question.hypothesis,
                "counter_hypothesis": question.counter_hypothesis,
                "why_material": question.why_material,
                "change_condition": question.weakens_if,
                "verdict": next(
                    assessment.verdict
                    for assessment in analysis.assessments
                    if assessment.question_id == question.question_id
                ),
            }
            for question in brief.questions
        ],
    }

    return GuruValidatedEvidenceAnalysis(
        brief_hash=brief.brief_hash,
        research_context_hash=_canonical_hash(research_context.model_dump(mode="json")),
        author_key=brief.author_key,
        ticker=brief.ticker.upper(),
        agent_analysis=analysis,
        decision_frame=decision_frame,
        validation={
            "all_questions_assessed": True,
            "evidence_is_question_scoped": True,
            "evidence_mode": "contextual",
            "allowed_verdicts": ["mixed", "unresolved"],
        },
    )


def _coerce_light_company_context(
    value: GuruCompanyOntologyContext | Mapping[str, Any] | None,
    *,
    ticker: str,
) -> GuruLightCompanyContext:
    if value is None:
        raise ValueError("company_context is required for an agent-generated investigation brief")
    if isinstance(value, GuruCompanyOntologyContext):
        raise ValueError(
            "agent-generated investigation briefs require GuruLightCompanyContext "
            "with trusted context_anchors"
        )
    payload = dict(value)
    aliases = {
        "companyName": "company_name",
        "businessDescription": "business_description",
        "primaryActivities": "primary_activities",
        "productsOrSegments": "products_or_segments",
        "revenueLogic": "revenue_logic",
        "contextAnchors": "context_anchors",
        "filingAvailability": "filing_availability",
    }
    normalized = {
        aliases.get(key, key): value
        for key, value in payload.items()
    }
    normalized_anchors: list[dict[str, Any]] = []
    for raw_anchor in normalized.get("context_anchors") or []:
        if not isinstance(raw_anchor, Mapping):
            continue
        normalized_anchors.append(
            {
                "anchor_id": raw_anchor.get("anchor_id") or raw_anchor.get("anchorId"),
                "kind": raw_anchor.get("kind"),
                "text": raw_anchor.get("text"),
            }
        )
    normalized["context_anchors"] = normalized_anchors
    normalized["ticker"] = normalized.get("ticker") or ticker
    context = GuruLightCompanyContext.model_validate(normalized)
    _require_nonempty_text(context.company_name, "company_context.company_name")
    _require_nonempty_text(context.business_description, "company_context.business_description")
    anchor_ids = [anchor.anchor_id.strip() for anchor in context.context_anchors]
    if len(anchor_ids) != len(set(anchor_ids)) or not all(anchor_ids):
        raise ValueError("company_context context_anchors must have unique non-empty anchor_id values")
    return context


def _normalize_investigation_draft(raw: Mapping[str, Any]) -> dict[str, Any]:
    aliases = {
        "guruPrincipleIds": "guru_principle_ids",
        "companyContextAnchorIds": "company_context_anchor_ids",
        "counterHypothesis": "counter_hypothesis",
        "evidenceNeeded": "evidence_needed",
        "strengthensIf": "strengthens_if",
        "weakensIf": "weakens_if",
        "whyMaterial": "why_material",
        "decisionRole": "decision_role",
    }
    return {aliases.get(key, key): value for key, value in dict(raw).items()}


def _normalize_investigation_brief(raw: Mapping[str, Any]) -> dict[str, Any]:
    aliases = {
        "briefHash": "brief_hash",
        "researchPackId": "research_pack_id",
        "authorKey": "author_key",
        "companyContextHash": "company_context_hash",
    }
    normalized = {aliases.get(key, key): value for key, value in dict(raw).items()}
    questions = normalized.get("questions")
    if isinstance(questions, Sequence) and not isinstance(questions, (str, bytes)):
        normalized["questions"] = [
            {
                **_normalize_investigation_draft(item),
                "question_id": item.get("question_id") or item.get("questionId"),
            }
            for item in questions
            if isinstance(item, Mapping)
        ]
    return normalized


def _normalize_agent_evidence_analysis(raw: Mapping[str, Any]) -> dict[str, Any]:
    aliases = {
        "overallJudgment": "overall_judgment",
    }
    normalized = {aliases.get(key, key): value for key, value in dict(raw).items()}
    assessments = normalized.get("assessments")
    if isinstance(assessments, Sequence) and not isinstance(assessments, (str, bytes)):
        normalized["assessments"] = [
            {
                "question_id": item.get("question_id") or item.get("questionId"),
                "evidence_object_ids": item.get("evidence_object_ids") or item.get("evidenceObjectIds") or [],
                "verdict": _normalize_evidence_verdict(item.get("verdict") or item.get("effect")),
                "reasoning": item.get("reasoning"),
            }
            for item in assessments
            if isinstance(item, Mapping)
        ]
    return {
        "assessments": normalized.get("assessments"),
        "overall_judgment": normalized.get("overall_judgment"),
    }


def _normalize_evidence_verdict(value: Any) -> Any:
    aliases = {
        "strengthens": "supported",
        "weakens": "mixed",
    }
    return aliases.get(value, value)


def _require_investigation_draft_fields(draft: GuruInvestigationQuestionDraft) -> None:
    for field_name, value in (
        ("question", draft.question),
        ("hypothesis", draft.hypothesis),
        ("counter_hypothesis", draft.counter_hypothesis),
        ("strengthens_if", draft.strengthens_if),
        ("weakens_if", draft.weakens_if),
        ("why_material", draft.why_material),
    ):
        _require_nonempty_text(value, f"investigation_questions.{field_name}")
    if draft.decision_role is None:
        raise ValueError(
            "investigation_questions.decision_role must be main_tension for the sole key question"
        )
    for field_name, values in (
        ("guru_principle_ids", draft.guru_principle_ids),
        ("company_context_anchor_ids", draft.company_context_anchor_ids),
        ("evidence_needed", draft.evidence_needed),
    ):
        if not values or any(not str(value).strip() for value in values):
            raise ValueError(f"investigation_questions.{field_name} must contain non-empty values")
        if len(values) != len(set(values)):
            raise ValueError(f"investigation_questions.{field_name} must not contain duplicates")


def _require_nonempty_text(value: Any, field_name: str) -> None:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{field_name} must be a non-empty string")


def _normalize_question_text(value: str) -> str:
    return " ".join(value.casefold().split())


def _sealed_question_id(
    *,
    author_key: str,
    ticker: str,
    question: GuruInvestigationQuestionDraft,
) -> str:
    payload = {
        "author_key": author_key,
        "ticker": ticker.upper(),
        "guru_principle_ids": sorted(question.guru_principle_ids),
        "company_context_anchor_ids": sorted(question.company_context_anchor_ids),
        "decision_role": question.decision_role,
        "question": _normalize_question_text(question.question),
    }
    return f"q_{_canonical_hash(payload)[:16]}"


def _canonical_hash(value: Mapping[str, Any]) -> str:
    encoded = json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _verified_evidence_ids_by_question(
    payload: Mapping[str, Any],
) -> dict[str, dict[str, Any]]:
    evidence_by_question = payload.get("evidence_by_question")
    if not isinstance(evidence_by_question, Sequence) or isinstance(
        evidence_by_question,
        (str, bytes),
    ):
        raise ValueError("company evidence payload is missing evidence_by_question")
    result: dict[str, dict[str, Any]] = {}
    for item in evidence_by_question:
        if not isinstance(item, Mapping):
            raise ValueError("company evidence contains an invalid question entry")
        question_id = _clean_optional(item.get("question_id"))
        answerability = _clean_optional(item.get("answerability"))
        evidence = item.get("evidence")
        if not question_id or answerability not in {
            "verified",
            "interpretation_only",
            "not_verified",
        }:
            raise ValueError("company evidence contains an invalid question status")
        if not isinstance(evidence, Sequence) or isinstance(evidence, (str, bytes)):
            raise ValueError("company evidence contains an invalid evidence list")
        object_ids = {
            str(entry.get("source_object_id"))
            for entry in evidence
            if isinstance(entry, Mapping) and entry.get("source_object_id")
        }
        if question_id in result:
            raise ValueError("company evidence contains duplicate question ids")
        result[question_id] = {
            "answerability": answerability,
            "object_ids": object_ids,
        }
    return result


def build_guru_company_research_pack(
    guru_payload: Mapping[str, Any],
    *,
    company_evidence_payload: Mapping[str, Any] | None = None,
    company_filing_brief: GuruCompanyFilingBrief | Mapping[str, Any] | None = None,
    ticker: str | None = None,
    company_name: str | None = None,
    company_context: GuruCompanyOntologyContext | Mapping[str, Any] | None = None,
    author_key: str | None = None,
    question: str | None = None,
) -> GuruCompanyResearchPack:
    """Join a Guru ResearchPack with optional opaque company evidence."""

    payload = _mapping(guru_payload)
    pack = _research_pack(payload)
    brief = _coerce_brief(
        company_filing_brief,
        guru_payload=payload,
        ticker=ticker,
        company_name=company_name,
        company_context=company_context,
        author_key=author_key,
        question=question,
    )
    company_pack = dict(company_evidence_payload or {})
    evidence_text = _json_text(company_pack)
    raw_selected_lenses = _list_of_mappings(pack.get("selected_lenses"))
    required_topics = list(brief.required_filing_topics)
    selected_lenses = _company_review_relevant_lenses(
        raw_selected_lenses,
        question=brief.original_question,
        evidence_text=evidence_text,
        required_topics=required_topics,
    )
    alignments = [
        _alignment_for_lens(lens, required_topics=required_topics, evidence_text=evidence_text)
        for lens in selected_lenses
    ]
    if not selected_lenses and required_topics:
        alignments.append(
            _alignment_for_lens(
                {
                    "company_evidence_requirements": required_topics,
                },
                required_topics=required_topics,
                evidence_text=evidence_text,
            )
        )
    missing_evidence = _missing_evidence(
        required_topics,
        evidence_text,
        has_company_pack=bool(company_pack),
    )
    labels = [str(lens.get("label_ko")) for lens in selected_lenses if lens.get("label_ko")]
    return GuruCompanyResearchPack(
        user_question=brief.original_question,
        author_key=brief.author_key,
        company_identity=brief.company_identity,
        guru_pack=dict(pack),
        company_context=dict(brief.company_context),
        company_filing_brief=brief.model_dump(mode="json", exclude_none=True),
        company_evidence_pack=company_pack,
        evidence_alignment=alignments,
        missing_evidence=missing_evidence,
        judgment_conditions={
            "can_discuss_now": labels,
            "needs_company_evidence_before_company_judgment": missing_evidence,
            "decision_change_triggers": _decision_change_triggers(missing_evidence),
        },
        render_hints={
            "author_key": brief.author_key,
            "voice_source": "skill-local answer-style.md plus Guru ResearchPack persona_profile",
            "use_first_person_simulated_voice": True,
            "avoid_footer_disclaimer": True,
            "separate_lens_from_company_facts": True,
        },
        answer_contract={
            "must_include": [
                "selected_guru_material",
                "filing_supported_company_facts_when_available",
                "missing_evidence_when_company_judgment_is_incomplete",
                "one_practical_next_question",
            ],
            "must_not_include": [
                "footer_disclaimer",
                "internal_tool_names",
                "ResearchPack_or_MCP_terms",
                "company_facts_not_present_in_company_evidence_pack",
            ],
        },
        boundaries=[
            "Guru pack supplies lens context only.",
            "Company evidence pack is opaque read-only filing evidence from the company research path.",
            "This pack does not mutate or extend the company ontology schema.",
        ],
    )


def build_guru_company_evidence_review(
    guru_payload: Mapping[str, Any],
    *,
    company_evidence_payload: Mapping[str, Any] | None = None,
    company_filing_brief: GuruCompanyFilingBrief | Mapping[str, Any] | None = None,
    ticker: str | None = None,
    company_name: str | None = None,
    company_context: GuruCompanyOntologyContext | Mapping[str, Any] | None = None,
    author_key: str | None = None,
    question: str | None = None,
) -> GuruCompanyEvidenceReview:
    """Review company evidence through the selected guru lenses.

    This is intentionally narrower than GuruCompanyResearchPack. It returns
    interpretation guidance for the final LLM, not a render plan or final
    prose template.
    """

    company_pack = build_guru_company_research_pack(
        guru_payload,
        company_evidence_payload=company_evidence_payload,
        company_filing_brief=company_filing_brief,
        ticker=ticker,
        company_name=company_name,
        company_context=company_context,
        author_key=author_key,
        question=question,
    )
    _validate_verified_question_ids(company_pack)
    alignments = list(company_pack.evidence_alignment)
    statuses = [alignment.status for alignment in alignments]
    supported = [
        alignment
        for alignment in alignments
        if alignment.status in {"supported", "partial", "not_required"}
    ]
    missing = [
        alignment
        for alignment in alignments
        if alignment.status in {"missing", "pending_company_research"}
    ]
    if supported and not missing:
        lens_alignment = "strengthens"
    elif supported and missing:
        lens_alignment = "mixed"
    elif statuses and all(status == "missing" for status in statuses):
        lens_alignment = "weakens"
    else:
        lens_alignment = "unresolved"

    emphasized_labels = _unique_strings(
        alignment.lens_label_ko
        for alignment in supported
        if alignment.lens_label_ko
    )
    weakened_topics = _unique_strings(
        _topic_ko(topic)
        for topic in company_pack.missing_evidence
    )
    selected_lenses = _list_of_mappings(company_pack.guru_pack.get("selected_lenses"))
    lens_summaries = _unique_strings(
        lens.get("summary_ko") or lens.get("label_ko")
        for lens in selected_lenses
        if lens.get("label_ko") in set(emphasized_labels)
    )
    question_items = _question_evidence_items(company_pack.company_evidence_pack)
    question_findings = _question_findings(question_items)
    question_overstatement_limits = _unique_strings(
        [
            *_question_overstatement_limits(question_items),
            *_dynamic_plan_overstatement_limits(company_pack.company_filing_brief),
        ]
    )
    primary_interpretation = _review_primary_interpretation(
        lens_alignment=lens_alignment,
        emphasized_labels=emphasized_labels,
        weakened_topics=weakened_topics,
    )
    advisor_question = _advisor_question_from_pack(
        company_pack.guru_pack,
        company_pack.company_identity.subject,
        allowed_lens_labels=set(emphasized_labels),
    )

    what_to_emphasize = _unique_strings(
        [
            *emphasized_labels,
            *lens_summaries[:3],
            *question_findings[:3],
            "회사 근거가 확인된 부분만 구루 렌즈와 연결한다.",
        ]
    )[:6]
    what_not_to_overstate = _unique_strings(
        [
            "CompanyEvidencePack에 없는 회사 사실이나 숫자를 새로 만들지 않는다.",
            "ResearchPack에 없는 구루 원칙, 유명 문구, 투자 규칙을 기억으로 보태지 않는다.",
            "고정된 보고서 섹션이나 표로 답변을 굳히지 않는다.",
            *question_overstatement_limits[:4],
            *[
                f"{topic} 근거가 부족하면 강한 결론으로 밀어붙이지 않는다."
                for topic in weakened_topics[:4]
            ],
        ]
    )
    change_conditions = (
        company_pack.judgment_conditions.get("decision_change_triggers")
        if isinstance(company_pack.judgment_conditions, Mapping)
        else None
    )
    if not change_conditions:
        change_conditions = [
            "선택된 구루 렌즈를 뒷받침하는 회사 근거가 약해질 때 판단을 다시 본다.",
            "부족한 회사 근거가 확인되거나 반대로 악화될 때 답변의 강도를 조정한다.",
        ]

    return GuruCompanyEvidenceReview(
        user_question=company_pack.user_question,
        author_key=company_pack.author_key,
        company_identity=company_pack.company_identity,
        lens_alignment=lens_alignment,
        primary_interpretation_ko=primary_interpretation,
        advisor_question_ko=advisor_question,
        strengthened_by=emphasized_labels,
        weakened_by=weakened_topics,
        what_to_emphasize=what_to_emphasize,
        what_not_to_overstate=what_not_to_overstate,
        change_conditions=_unique_strings(change_conditions)[:6],
        missing_evidence=company_pack.missing_evidence,
        evidence_alignment=alignments,
        answer_contract={
            "purpose": "final_answer_guidance_only",
            "write_freely": True,
            "must_use": [
                "Guru ResearchPack selected lenses",
                "dynamic question answers when available",
                "CompanyEvidencePack filing facts",
                "this review's emphasis and overstatement limits",
            ],
            "must_not_include": [
                "fixed_report_template",
                "internal_tool_names",
                "ResearchPack_or_CompanyEvidencePack_terms",
                "company_facts_not_present_in_company_evidence_pack",
                "guru_principles_absent_from_research_pack",
            ],
        },
        boundaries=[
            "Review guides interpretation only; it is not final prose.",
            "Final answer should sound like a consultation, not a structured analyst report.",
            "Guru ontology supplies thinking lens; company evidence pack supplies company facts.",
        ],
    )


def _question_evidence_items(company_evidence_pack: Mapping[str, Any]) -> list[Mapping[str, Any]]:
    raw = company_evidence_pack.get("evidence_by_question")
    if isinstance(raw, Mapping):
        items: list[Mapping[str, Any]] = []
        for key, value in raw.items():
            if not isinstance(value, Mapping):
                continue
            if "question_en" in value or "question_ko_label" in value:
                items.append(value)
            else:
                items.append({"question_en": str(key), **value})
        return items
    if not isinstance(raw, Sequence) or isinstance(raw, (str, bytes)):
        return []
    return [item for item in raw if isinstance(item, Mapping)]


def _question_findings(items: Sequence[Mapping[str, Any]]) -> list[str]:
    values: list[str] = []
    for item in items:
        verified_evidence = item.get("evidence")
        if isinstance(verified_evidence, Sequence) and not isinstance(
            verified_evidence, (str, bytes)
        ):
            for evidence_item in verified_evidence:
                if not isinstance(evidence_item, Mapping):
                    continue
                excerpt = _clean_optional(evidence_item.get("verified_excerpt"))
                document = (
                    evidence_item.get("document")
                    if isinstance(evidence_item.get("document"), Mapping)
                    else {}
                )
                if not excerpt:
                    continue
                source_label = " ".join(
                    str(value)
                    for value in (
                        document.get("period"),
                        document.get("document_type"),
                    )
                    if value
                )
                values.append(f"{source_label}: {excerpt}" if source_label else excerpt)
            continue
        effect = str(item.get("lens_effect") or "").lower()
        finding = _clean_optional(item.get("finding"))
        if not finding:
            continue
        if effect in {"supports", "weakens", "mixed", "unknown"}:
            values.append(finding)
    return _unique_strings(values)


def _question_overstatement_limits(items: Sequence[Mapping[str, Any]]) -> list[str]:
    values: list[str] = []
    for item in items:
        answerability = _clean_optional(item.get("answerability"))
        if answerability == "interpretation_only":
            values.append("이 질문의 근거는 해석 보조용이므로 직접 공시 사실처럼 단정하지 않는다.")
        elif answerability == "not_verified":
            values.append("이 질문은 검증된 원문 근거가 없어 결론 강도를 낮춘다.")
        limit = _clean_optional(item.get("do_not_overstate"))
        if limit:
            values.append(limit)
        missing = _clean_optional(item.get("missing_evidence"))
        if missing:
            values.append(f"{missing} 근거가 비어 있으면 결론 강도를 낮춘다.")
    return _unique_strings(values)


def _validate_verified_question_ids(company_pack: GuruCompanyResearchPack) -> None:
    evidence_pack = company_pack.company_evidence_pack
    if evidence_pack.get("format") != "krw-verified-company-evidence/v1":
        return
    filing_brief = company_pack.company_filing_brief
    plan = filing_brief.get("dynamic_question_plan")
    if not isinstance(plan, Mapping):
        return
    expected_questions = plan.get("questions")
    if not isinstance(expected_questions, Sequence) or isinstance(
        expected_questions, (str, bytes)
    ):
        return
    expected_ids = {
        str(item.get("question_id"))
        for item in expected_questions
        if isinstance(item, Mapping) and item.get("question_id")
    }
    actual_ids = {
        str(item.get("question_id"))
        for item in _question_evidence_items(evidence_pack)
        if item.get("question_id")
    }
    if expected_ids and actual_ids != expected_ids:
        raise ValueError(
            "verified company evidence question ids do not match the dynamic question plan"
        )


def _dynamic_plan_overstatement_limits(
    company_filing_brief: Mapping[str, Any],
) -> list[str]:
    plan = company_filing_brief.get("dynamic_question_plan")
    if not isinstance(plan, Mapping):
        return []
    questions = plan.get("questions")
    if not isinstance(questions, Sequence) or isinstance(questions, (str, bytes)):
        return []
    return _unique_strings(
        item.get("do_not_overstate")
        for item in questions
        if isinstance(item, Mapping) and item.get("do_not_overstate")
    )


def _review_primary_interpretation(
    *,
    lens_alignment: str,
    emphasized_labels: Sequence[str],
    weakened_topics: Sequence[str],
) -> str:
    first_label = emphasized_labels[0] if emphasized_labels else "선택된 구루 렌즈"
    if lens_alignment == "strengthens":
        return f"{first_label} 관점은 현재 회사 근거로 어느 정도 뒷받침됩니다. 다만 최종 답변은 숫자 나열보다 이 근거가 투자 가정을 어떻게 강화하는지 설명해야 합니다."
    if lens_alignment == "mixed":
        weak_text = ", ".join(weakened_topics[:3]) or "일부 회사 근거"
        return f"{first_label} 관점은 일부 뒷받침되지만 {weak_text} 쪽은 아직 과장하면 안 됩니다. 상담 답변은 확신과 한계를 함께 보여줘야 합니다."
    if lens_alignment == "weakens":
        weak_text = ", ".join(weakened_topics[:3]) or "필요한 회사 근거"
        return f"현재 회사 근거는 선택된 구루 렌즈를 강하게 밀어주지 못합니다. 특히 {weak_text}가 약하면 결론을 보수적으로 낮춰야 합니다."
    return "현재 회사 근거만으로는 선택된 구루 렌즈를 강하게 단정하기 어렵습니다. 답변은 판단보다 확인해야 할 가정과 조건을 먼저 세워야 합니다."


def _advisor_question_from_pack(
    pack: Mapping[str, Any],
    subject: str,
    *,
    allowed_lens_labels: set[str] | None = None,
) -> str | None:
    for row in _list_of_mappings(pack.get("consultation_moves")):
        question = _clean_optional(row.get("question_pattern_ko") or row.get("prompt_ko"))
        if question:
            return question.replace("{ticker}", subject).replace("{company}", subject)
    for row in _list_of_mappings(pack.get("consultation_objects")):
        question = _clean_optional(row.get("question_pattern_ko") or row.get("prompt_ko"))
        if question:
            return question.replace("{ticker}", subject).replace("{company}", subject)
    for lens in _list_of_mappings(pack.get("selected_lenses")):
        label = _clean_optional(lens.get("label_ko"))
        if label and (allowed_lens_labels is None or label in allowed_lens_labels):
            return f"{subject}에서 {label}이 실제 회사 근거로 확인되는가?"
    return None


def _coerce_brief(
    value: GuruCompanyFilingBrief | Mapping[str, Any] | None,
    *,
    guru_payload: Mapping[str, Any],
    ticker: str | None,
    company_name: str | None,
    company_context: GuruCompanyOntologyContext | Mapping[str, Any] | None,
    author_key: str | None,
    question: str | None,
) -> GuruCompanyFilingBrief:
    if isinstance(value, GuruCompanyFilingBrief):
        return value
    if isinstance(value, Mapping):
        return GuruCompanyFilingBrief.model_validate(value)
    return company_filing_brief_from_guru_lens(
        guru_payload,
        ticker=ticker,
        company_name=company_name,
        company_context=company_context,
        author_key=author_key,
        question=question,
    )


def _alignment_for_lens(
    lens: Mapping[str, Any],
    *,
    required_topics: Sequence[str],
    evidence_text: str,
) -> GuruCompanyEvidenceAlignment:
    lens_requirements = _unique_strings(
        lens.get("company_evidence_requirements")
        or lens.get("conditional_company_evidence_requirements")
        or required_topics
    )
    if not lens_requirements:
        return GuruCompanyEvidenceAlignment(
            lens_id=_clean_optional(lens.get("reviewed_id")),
            lens_label_ko=_clean_optional(lens.get("label_ko")),
            status="not_required",
            reason_ko="이 렌즈는 현재 회사 공시 근거를 직접 요구하지 않습니다.",
        )
    if not evidence_text:
        status = "pending_company_research"
        reason = "회사 공시 근거가 아직 조립되지 않았습니다."
    else:
        supported = [topic for topic in lens_requirements if _topic_supported(topic, evidence_text)]
        if len(supported) == len(lens_requirements):
            status = "supported"
            reason = "필요한 회사 공시 근거가 모두 연결되었습니다."
        elif supported:
            status = "partial"
            reason = "일부 회사 공시 근거만 연결되었습니다."
        else:
            status = "missing"
            reason = "필요한 회사 공시 근거를 현재 payload에서 찾지 못했습니다."
    return GuruCompanyEvidenceAlignment(
        lens_id=_clean_optional(lens.get("reviewed_id")),
        lens_label_ko=_clean_optional(lens.get("label_ko")),
        required_company_evidence=list(lens_requirements),
        status=status,
        reason_ko=reason,
    )


def _company_review_relevant_lenses(
    lenses: Sequence[Mapping[str, Any]],
    *,
    question: str,
    evidence_text: str,
    required_topics: Sequence[str],
) -> list[Mapping[str, Any]]:
    """Keep only lenses that actually connect to the question/evidence context.

    Query-context retrieval can return broad author lenses that share generic
    filing needs such as business_model or risk_factors. That is useful for
    discovery, but final company review should not over-emphasize a lens unless
    its own wording or data hooks overlap the specific company question.
    """

    if not lenses:
        return []
    context_tokens = _text_tokens(" ".join([question, evidence_text, *required_topics]))
    scored: list[tuple[int, int, Mapping[str, Any]]] = []
    for index, lens in enumerate(lenses):
        score = _lens_relevance_score(lens, context_tokens=context_tokens)
        if score >= 2:
            scored.append((score, -index, lens))
    scored.sort(reverse=True, key=lambda row: (row[0], row[1]))
    return [lens for _, _, lens in scored[:4]]


def _lens_relevance_score(
    lens: Mapping[str, Any],
    *,
    context_tokens: set[str],
) -> int:
    lens_tokens = _text_tokens(
        " ".join(
            [
                _json_text(
                    {
                        key: lens.get(key)
                        for key in (
                            "label_ko",
                            "label_en",
                            "summary_ko",
                            "summary_en",
                            "interpretation_ko",
                            "interpretation_en",
                            "strong_for",
                            "weak_for",
                            "anti_triggers",
                            "question_pattern_ko",
                            "question_pattern_en",
                            "company_evidence_requirements",
                            "conditional_company_evidence_requirements",
                            "related_data_needs",
                        )
                    }
                )
            ]
        )
    )
    if not lens_tokens or not context_tokens:
        return 0
    common = lens_tokens & context_tokens
    score = len(common)
    if {"moat", "해자"} & common:
        score += 2
    if {"service", "services", "서비스", "recurring", "반복"} & common:
        score += 2
    if {"iphone", "아이폰"} & common:
        score += 2
    if {"cash", "flow", "현금흐름", "owner", "earnings"} & common:
        score += 1
    return score


def _text_tokens(text: str) -> set[str]:
    tokens: set[str] = set()
    for token in re.findall(r"[a-z0-9_]+|[가-힣]+", text.lower()):
        if len(token) < 2:
            continue
        tokens.add(token)
        if token.endswith("s") and len(token) > 3:
            tokens.add(token[:-1])
    return tokens


def _missing_evidence(
    required_topics: Sequence[str],
    evidence_text: str,
    *,
    has_company_pack: bool,
) -> list[str]:
    if not has_company_pack:
        return list(required_topics)
    return [topic for topic in required_topics if not _topic_supported(topic, evidence_text)]


def _filter_filing_topics(
    *,
    topics: Sequence[str],
    question: str,
    subject: str,
    company_context: GuruCompanyOntologyContext | Mapping[str, Any] | None,
    generic_requirements: Sequence[str],
) -> dict[str, Any]:
    required: list[str] = []
    filtered: list[dict[str, str]] = []
    generic_set = set(generic_requirements)
    context_tags = _context_tags(question, subject, company_context)
    for topic in topics:
        if topic in generic_set:
            required.append(topic)
            continue
        reason = _topic_filter_reason(topic, context_tags=context_tags)
        if reason:
            filtered.append({"topic": topic, "reason": reason})
            continue
        required.append(topic)
    return {
        "required": _unique_strings(required),
        "filtered": filtered,
    }


def _filter_query_data_need_keys(
    keys: Sequence[str],
    *,
    question: str,
    subject: str,
    company_context: GuruCompanyOntologyContext | Mapping[str, Any] | None,
) -> list[str]:
    context_tags = _context_tags(question, subject, company_context)
    return [
        key
        for key in keys
        if not _topic_filter_reason(key, context_tags=context_tags)
    ]


def _topic_filter_reason(topic: str, *, context_tags: set[str]) -> str | None:
    reason = filing_topic_filter_reason(topic, context_tags=context_tags)
    if reason:
        return reason
    if len(topic) > 48 and re.search(r"[가-힣]", topic):
        return "topic is a long natural-language hook; keep it as candidate context, not a required filing topic"
    return None


def _context_tags(
    question: str,
    subject: str,
    company_context: GuruCompanyOntologyContext | Mapping[str, Any] | None,
) -> set[str]:
    context_model = (
        company_context
        if isinstance(company_context, GuruCompanyOntologyContext)
        else coerce_company_context(company_context)
    )
    context_text = ""
    if context_model is not None:
        context_text = " ".join(
            [
                *context_model.context_terms,
                *context_model.business_context_terms,
                *context_model.risk_context_terms,
                *context_model.available_company_topics,
                *context_model.context_tags,
            ]
        )
    return context_tags_for_text(f"{question} {subject} {context_text}")


def _decision_change_triggers(missing_evidence: Sequence[str]) -> list[str]:
    return [
        f"{_topic_ko(topic)} 근거가 확인되거나 반대로 악화될 때 판단을 다시 봅니다."
        for topic in missing_evidence[:6]
    ]


def _research_pack(payload: Mapping[str, Any]) -> dict[str, Any]:
    pack = payload.get("research_pack")
    if isinstance(pack, Mapping):
        return dict(pack)
    if payload.get("format") == "krw-guru-research-pack/v1":
        return dict(payload)
    return dict(payload)


def _author_key(author_key: str | None, pack: Mapping[str, Any]) -> str:
    if author_key in AUTHOR_KEYS:
        return str(author_key)
    guru_keys = _get_nested(pack, ("pack_meta", "guru_keys"))
    if isinstance(guru_keys, Sequence) and not isinstance(guru_keys, (str, bytes)):
        for key in guru_keys:
            if key in AUTHOR_KEYS:
                return str(key)
    return "buffett"


def _company_subject(ticker: str | None, company_name: str | None) -> str:
    if ticker and company_name:
        return f"{ticker} {company_name}"
    if ticker:
        return ticker
    if company_name:
        return company_name
    return "selected company"


def _lens_specific_evidence_requests(pack: Mapping[str, Any]) -> list[str]:
    values: list[str] = []
    for row in _list_of_mappings(pack.get("data_needs")):
        values.extend(_normalized_hooks(row.get("company_data_hooks") or []))
        key = _clean_optional(row.get("data_need_key"))
        if key:
            values.append(key)
    for lens in _list_of_mappings(pack.get("selected_lenses")):
        values.extend(_unique_strings(lens.get("company_evidence_requirements") or []))
        values.extend(_unique_strings(lens.get("conditional_company_evidence_requirements") or []))
        for need in _list_of_mappings(lens.get("related_data_needs")):
            values.extend(_normalized_hooks(need.get("company_data_hooks") or []))
            key = _clean_optional(need.get("data_need_key"))
            if key:
                values.append(key)
    return _unique_strings(values)


def _normalized_hooks(values: Any) -> list[str]:
    hooks: list[str] = []
    if not isinstance(values, Iterable) or isinstance(values, (str, bytes)):
        return _unique_strings(values)
    for value in values:
        if isinstance(value, Mapping):
            hook = value.get("key") or value.get("metric") or value.get("section") or value.get("topic")
            if hook:
                hooks.append(str(hook))
        else:
            hooks.append(str(value))
    return _unique_strings(hooks)


def _data_need_keys(pack: Mapping[str, Any]) -> list[str]:
    keys = [_clean_optional(row.get("data_need_key")) for row in _list_of_mappings(pack.get("data_needs"))]
    for lens in _list_of_mappings(pack.get("selected_lenses")):
        keys.extend(
            _clean_optional(row.get("data_need_key"))
            for row in _list_of_mappings(lens.get("related_data_needs"))
        )
    return _unique_strings(key for key in keys if key)


def _query_terms(
    *,
    subject: str,
    ticker: str | None,
    company_name: str | None,
    topics: Sequence[str],
    data_need_keys: Sequence[str],
) -> list[str]:
    terms: list[str] = [subject]
    if ticker:
        terms.append(ticker)
    if company_name:
        terms.append(company_name)
    for topic in topics:
        terms.append(topic)
        terms.extend(TOPIC_QUERY_EXPANSIONS.get(topic, ()))
    terms.extend(data_need_keys)
    return _unique_strings(term for term in terms if term)[:40]


def _company_question_ko(*, subject: str, original_question: str, topics: Sequence[str]) -> str:
    topic_text = ", ".join(_topic_ko(topic) for topic in topics[:12]) or "사업 모델, 현금흐름, 자본배분, 재무 안정성, 리스크 요인"
    return (
        f"{subject} 공시에서 {topic_text}를 확인하라. "
        f"구루 상담 질문에 필요한 회사 사실만 가져오고, 목표가나 매수/매도 결론은 만들지 않는다. "
        f"사용자 질문: {original_question}"
    )


def _company_question_en(
    *,
    subject: str,
    original_question: str,
    topics: Sequence[str],
    query_terms: Sequence[str],
) -> str:
    topic_text = ", ".join(topics[:12]) or "business_model, cash_flow, capital_allocation, balance_sheet, risk_factors"
    query_text = ", ".join(query_terms[:24])
    return (
        f"For {subject}, retrieve filing-grounded evidence for: {topic_text}. "
        f"Useful query terms: {query_text}. "
        f"Return company facts only; do not produce a price target or buy/sell conclusion. "
        f"User question: {original_question}"
    )


def _dynamic_question_plan(
    *,
    author_key: str,
    subject: str,
    ticker: str | None,
    company_name: str | None,
    original_question: str,
    pack: Mapping[str, Any],
    company_context: GuruCompanyOntologyContext | Mapping[str, Any] | None,
    required_topics: Sequence[str],
    candidate_topics: Sequence[str],
    data_need_keys: Sequence[str],
    query_terms: Sequence[str],
    intent_family: str | None,
) -> GuruDynamicQuestionPlan | None:
    """Build company-specific questions that a filing subagent should answer.

    These questions are intentionally deterministic. The main guru LLM should
    interpret them, but the serving layer should define the evidence lanes so
    company research does not degrade into a generic analyst summary.
    """

    if not subject or subject == "selected company":
        return None
    context_terms = _company_specific_terms(company_context)
    topics = _question_plan_topics(required_topics, candidate_topics, data_need_keys)
    if not topics:
        topics = ["business_model", "cash_flow", "capital_allocation", "risk_factors"]
    selected_lenses = _list_of_mappings(pack.get("selected_lenses"))
    questions: list[GuruDynamicQuestionPlanItem] = []
    for topic in topics:
        if len(questions) >= 5:
            break
        item = _dynamic_question_for_topic(
            author_key=author_key,
            subject=subject,
            topic=topic,
            selected_lenses=selected_lenses,
            context_terms=context_terms,
            query_terms=query_terms,
            original_question=original_question,
            index=len(questions),
        )
        if item is None:
            continue
        if _is_generic_question(item.question_en, subject=subject, context_terms=context_terms):
            continue
        questions.append(item)
    if len(questions) < 3:
        for fallback_topic in ("business_model", "cash_flow", "risk_factors", "capital_allocation"):
            if any(fallback_topic in item.evidence_needed for item in questions):
                continue
            item = _dynamic_question_for_topic(
                author_key=author_key,
                subject=subject,
                topic=fallback_topic,
                selected_lenses=selected_lenses,
                context_terms=context_terms,
                query_terms=query_terms,
                original_question=original_question,
                index=len(questions),
            )
            if item is not None:
                questions.append(item)
            if len(questions) >= 3:
                break
    if not questions:
        return None
    return GuruDynamicQuestionPlan(
        author_key=_author_key(author_key, pack),
        ticker=ticker,
        company_name=company_name,
        subject=subject,
        user_intent=intent_family,
        company_context_terms=context_terms[:12],
        questions=questions[:5],
        construction_rules=[
            "Questions are generated from the selected guru ResearchPack, runtime company context, and filing topics.",
            "Each question must be company-specific; if replacing the subject with another ticker still sounds natural, rewrite it.",
            "The company evidence subagent answers these questions with latest 10-Q/current-driver evidence first and latest 10-K as baseline.",
            "The final Guru answer uses the answers as internal material only and must not expose this plan.",
        ],
    )


def _question_plan_topics(
    required_topics: Sequence[str],
    candidate_topics: Sequence[str],
    data_need_keys: Sequence[str],
) -> list[str]:
    priority = [
        "business_model",
        "cash_flow",
        "cash_generation",
        "capital_allocation",
        "share_repurchases",
        "balance_sheet",
        "risk_factors",
        "valuation_context",
        "margin_structure",
        "growth_duration",
        "demand_cycle_exposure",
        "margin_pressure",
        "liquidity",
        "capital_intensity",
        "services_mix",
        "customer_demand",
        "commodity_price_exposure",
    ]
    raw = _unique_strings([*required_topics, *candidate_topics, *data_need_keys])
    normalized: list[str] = []
    for topic in raw:
        clean = _clean_optional(topic)
        if not clean:
            continue
        normalized.append(clean)
    ordered = [topic for topic in priority if topic in set(normalized)]
    extras = [topic for topic in normalized if topic not in set(ordered)]
    return _unique_strings([*ordered, *extras])[:10]


def _dynamic_question_for_topic(
    *,
    author_key: str,
    subject: str,
    topic: str,
    selected_lenses: Sequence[Mapping[str, Any]],
    context_terms: Sequence[str],
    query_terms: Sequence[str],
    original_question: str,
    index: int,
) -> GuruDynamicQuestionPlanItem | None:
    template = TOPIC_QUESTION_TEMPLATES.get(topic)
    if template:
        question_template, label, do_not_overstate = template
    else:
        label = f"{_topic_ko(topic)} 근거가 판단을 바꾸나?"
        question_template = (
            "For {subject}, what does filing evidence say about "
            f"{topic}, and how should that change the selected guru's judgment?"
        )
        do_not_overstate = f"Do not make a strong claim about {topic} unless filing evidence directly supports it."
    context_phrase = _context_phrase(context_terms)
    question_en = question_template.format(subject=subject)
    if context_phrase and context_phrase.lower() not in question_en.lower():
        question_en = f"{question_en} Pay special attention to {context_phrase}."
    lens = _best_lens_for_topic(selected_lenses, topic=topic, original_question=original_question)
    evidence_needed = _unique_strings(
        [
            topic,
            *TOPIC_QUERY_EXPANSIONS.get(topic, ()),
            *_lens_evidence_terms(lens),
        ]
    )[:8]
    priority: str = "highest" if index == 0 else "high" if index < 3 else "medium"
    answer_role = _answer_role_for_index(index, topic=topic)
    retrieval_terms = _unique_strings(
        [
            subject,
            topic,
            *TOPIC_QUERY_EXPANSIONS.get(topic, ()),
            *context_terms[:6],
            *query_terms[:10],
            "latest 10-Q",
            "latest 10-K",
        ]
    )
    why_guru = _why_guru_relevant(author_key=author_key, lens=lens, topic=topic)
    why_company = _why_company_specific(subject=subject, context_terms=context_terms, topic=topic)
    return GuruDynamicQuestionPlanItem(
        question_id=_dynamic_question_id(
            author_key=author_key,
            subject=subject,
            topic=topic,
            question_en=question_en,
        ),
        question_en=question_en,
        question_ko_label=label,
        why_guru_relevant=why_guru,
        why_company_specific=why_company,
        evidence_needed=evidence_needed,
        priority=priority,  # type: ignore[arg-type]
        answer_role=answer_role,  # type: ignore[arg-type]
        retrieval_query_en=" ".join(retrieval_terms[:18]),
        stop_condition=(
            "Stop after the latest 10-Q/current driver and latest 10-K baseline show "
            "whether the question is supported, weakened, mixed, or not answerable from filings."
        ),
        do_not_overstate=do_not_overstate,
    )


def _dynamic_question_id(
    *,
    author_key: str,
    subject: str,
    topic: str,
    question_en: str,
) -> str:
    canonical = "|".join(
        " ".join(value.lower().split())
        for value in (author_key, subject, topic, question_en)
    )
    return f"q_{hashlib.sha256(canonical.encode('utf-8')).hexdigest()[:12]}"


def _company_specific_terms(
    company_context: GuruCompanyOntologyContext | Mapping[str, Any] | None,
) -> list[str]:
    if company_context is None:
        return []
    model = (
        company_context
        if isinstance(company_context, GuruCompanyOntologyContext)
        else coerce_company_context(company_context)
    )
    if model is None:
        return []
    return _unique_strings(
        [
            *model.context_terms,
            *model.business_context_terms,
            *model.risk_context_terms,
            *model.available_company_topics,
            *model.context_tags,
        ]
    )


def _context_phrase(context_terms: Sequence[str]) -> str:
    visible = [
        term
        for term in context_terms
        if term and len(term) <= 80 and not term.startswith("company_ontology")
    ]
    return ", ".join(visible[:4])


def _best_lens_for_topic(
    lenses: Sequence[Mapping[str, Any]],
    *,
    topic: str,
    original_question: str,
) -> Mapping[str, Any]:
    if not lenses:
        return {}
    topic_tokens = _text_tokens(" ".join([topic, _topic_ko(topic), original_question]))
    scored: list[tuple[int, int, Mapping[str, Any]]] = []
    for index, lens in enumerate(lenses):
        text = _json_text(
            {
                key: lens.get(key)
                for key in (
                    "label_ko",
                    "label_en",
                    "summary_ko",
                    "summary_en",
                    "interpretation_ko",
                    "interpretation_en",
                    "company_evidence_requirements",
                    "conditional_company_evidence_requirements",
                    "related_data_needs",
                )
            }
        )
        lens_tokens = _text_tokens(text)
        overlap = len(topic_tokens & lens_tokens)
        if topic in _json_text(lens):
            overlap += 4
        scored.append((overlap, -index, lens))
    scored.sort(reverse=True, key=lambda row: (row[0], row[1]))
    return scored[0][2]


def _lens_evidence_terms(lens: Mapping[str, Any]) -> list[str]:
    terms: list[str] = []
    terms.extend(_unique_strings(lens.get("company_evidence_requirements") or []))
    terms.extend(_unique_strings(lens.get("conditional_company_evidence_requirements") or []))
    for need in _list_of_mappings(lens.get("related_data_needs")):
        terms.extend(_normalized_hooks(need.get("company_data_hooks") or []))
        key = _clean_optional(need.get("data_need_key"))
        if key:
            terms.append(key)
    return _unique_strings(terms)


def _why_guru_relevant(
    *,
    author_key: str,
    lens: Mapping[str, Any],
    topic: str,
) -> str:
    label = _clean_optional(lens.get("label_en") or lens.get("label_ko"))
    summary = _clean_optional(lens.get("summary_en") or lens.get("summary_ko"))
    if label and summary:
        return f"{AUTHOR_DISPLAY_NAME_FOR_BRIDGE.get(author_key, author_key)} lens '{label}' needs this because {summary}"
    if label:
        return f"{AUTHOR_DISPLAY_NAME_FOR_BRIDGE.get(author_key, author_key)} lens '{label}' needs filing evidence for {_topic_ko(topic)}."
    return f"The selected guru lens needs filing evidence for {_topic_ko(topic)} before making a company judgment."


def _why_company_specific(
    *,
    subject: str,
    context_terms: Sequence[str],
    topic: str,
) -> str:
    if context_terms:
        return f"{subject} is being evaluated through company-specific context: {', '.join(context_terms[:5])}."
    return f"{subject} is the selected company, so {_topic_ko(topic)} must be checked in its own latest filings rather than from a generic guru principle."


def _answer_role_for_index(index: int, *, topic: str) -> str:
    if index == 0:
        return "main_tension"
    if topic in {"risk_factors", "balance_sheet", "liquidity", "margin_pressure"}:
        return "counterweight"
    if topic in {"valuation_context", "growth_duration", "capital_allocation"}:
        return "change_condition"
    if index >= 5:
        return "do_not_overstate"
    return "supporting_evidence"


def _is_generic_question(
    question: str,
    *,
    subject: str,
    context_terms: Sequence[str],
) -> bool:
    lowered = question.lower()
    subject_terms = _text_tokens(subject)
    if subject_terms and subject_terms & _text_tokens(question):
        return False
    context_tokens = _text_tokens(" ".join(context_terms))
    return not bool(context_tokens & _text_tokens(lowered))


def _missing_inputs(
    *,
    requires_company: bool,
    requires_identifier: bool,
    ticker: str | None,
    company_name: str | None,
) -> list[str]:
    if requires_identifier:
        return ["exact_ticker_exchange_share_class"]
    if requires_company and not ticker and not company_name:
        return ["ticker_or_company_name"]
    return []


def _topic_supported(topic: str, evidence_text: str) -> bool:
    if not evidence_text:
        return False
    normalized_topic = topic.replace("_", " ").lower()
    keywords = [normalized_topic, topic.lower(), *TOPIC_QUERY_EXPANSIONS.get(topic, ())]
    return any(str(keyword).lower() in evidence_text for keyword in keywords if keyword)


def _topic_ko(topic: str) -> str:
    return TOPIC_KO_LABELS.get(topic, topic.replace("_", " "))


def _mapping(value: Any) -> Mapping[str, Any]:
    return value if isinstance(value, Mapping) else {}


def _list_of_mappings(value: Any) -> list[Mapping[str, Any]]:
    if not isinstance(value, Sequence) or isinstance(value, (str, bytes)):
        return []
    return [item for item in value if isinstance(item, Mapping)]


def _get_nested(value: Mapping[str, Any], path: Sequence[str]) -> Any:
    current: Any = value
    for key in path:
        if not isinstance(current, Mapping):
            return None
        current = current.get(key)
    return current


def _unique_strings(values: Any) -> list[str]:
    result: list[str] = []
    if isinstance(values, (str, bytes)):
        values = [values]
    if not isinstance(values, Iterable):
        return result
    for value in values:
        text = _clean_optional(value)
        if text and text not in result:
            result.append(text)
    return result


def _clean_optional(value: Any) -> str | None:
    if value is None:
        return None
    text = re.sub(r"\s+", " ", str(value)).strip()
    return text or None


def _json_text(value: Any) -> str:
    if not value:
        return ""
    return json.dumps(value, ensure_ascii=False, sort_keys=True).lower()
