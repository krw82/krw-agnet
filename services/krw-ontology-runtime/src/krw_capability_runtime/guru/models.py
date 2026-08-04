"""Data contracts for the standalone guru-letter ontology pipeline.

These models intentionally do not extend the company filing ontology schema.
They describe a separate lens ontology built from investor letters and memos.
"""

from __future__ import annotations

from datetime import datetime, timezone
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field


GURU_ONTOLOGY_SCHEMA_VERSION = "krw-guru-ontology/v1"
GURU_SOURCE_MANIFEST_FORMAT = "krw-guru-source-manifest/v1"
GURU_COLLECTION_PLAN_FORMAT = "krw-guru-collection-plan/v1"
GURU_ONTOLOGY_MANIFEST_FORMAT = "krw-guru-ontology-manifest/v1"
GURU_DISCOVERY_MANIFEST_FORMAT = "krw-guru-discovery-manifest/v1"
GURU_RAW_MANIFEST_FORMAT = "krw-guru-raw-manifest/v1"
GURU_PARSED_MANIFEST_FORMAT = "krw-guru-parsed-manifest/v1"
GURU_EXTRACTION_MANIFEST_FORMAT = "krw-guru-extraction-manifest/v1"
GURU_CURATION_REPORT_FORMAT = "krw-guru-curation-report/v1"
GURU_RESEARCH_PACK_FORMAT = "krw-guru-research-pack/v1"
GURU_COMPANY_ONTOLOGY_CONTEXT_FORMAT = "krw-guru-company-ontology-context/v1"
GURU_COMPANY_FILING_BRIEF_FORMAT = "krw-guru-company-filing-brief/v1"
GURU_DYNAMIC_QUESTION_PLAN_FORMAT = "krw-guru-dynamic-question-plan/v1"
GURU_LIGHT_COMPANY_CONTEXT_FORMAT = "krw-guru-light-company-context/v1"
GURU_INVESTIGATION_BRIEF_FORMAT = "krw-guru-investigation-brief/v1"
GURU_COMPANY_RESEARCH_CONTEXT_FORMAT = "krw-guru-company-research-context/v1"
GURU_VALIDATED_EVIDENCE_ANALYSIS_FORMAT = "krw-guru-validated-evidence-analysis/v1"
GURU_COMPANY_RESEARCH_PACK_FORMAT = "krw-guru-company-research-pack/v1"
GURU_COMPANY_EVIDENCE_REVIEW_FORMAT = "krw-guru-company-evidence-review/v1"
GURU_ANSWER_RENDER_PLAN_FORMAT = "krw-guru-answer-render-plan/v1"

AUTHOR_KEYS = ("buffett", "marks", "ackman", "flatt", "terry_smith")
SOURCE_TYPES = ("shareholder_letter", "memo")
RIGHTS_POLICIES = (
    "official_link_only_no_fulltext",
    "public_excerpt_limited",
    "internal_summary_only",
)
OBJECT_STATUSES = ("candidate", "reviewed", "rejected")
CONFIDENCE_LEVELS = ("low", "medium", "high")
SOFT_METADATA_CONFIDENCE_LEVELS = ("low", "medium", "high")
SPECIFICITY_LEVELS = (
    "general_principle",
    "sector_specific",
    "asset_class_specific",
    "company_case_specific",
    "document_context_specific",
)
ANSWER_ROLES = (
    "core_lens",
    "supporting_lens",
    "caution",
    "checklist",
    "data_need",
    "contrast",
    "context",
)
SEARCH_TARGET_TYPES = ("metric", "topic", "section")
SAFETY_SEVERITIES = ("warning", "error")
RAW_DOCUMENT_STATUSES = ("fetched", "skipped", "error")
PARSED_DOCUMENT_STATUSES = ("parsed", "skipped", "error")
EXTRACTION_EXECUTION_MODES = ("dry_run", "agent_sdk")
INVESTOR_INTENT_FAMILIES = (
    "learn_guru_view",
    "considering_buy",
    "already_bought",
    "holding_review",
    "sell_or_trim",
    "average_down",
    "risk_check",
    "valuation_check",
    "business_quality_check",
    "moat_check",
    "management_check",
    "capital_allocation_check",
    "balance_sheet_check",
    "cyclical_risk_check",
    "portfolio_fit",
    "position_sizing",
    "compare_candidates",
    "contrarian_check",
    "red_flag_check",
    "thesis_review",
    "post_earnings_review",
    "loss_recovery",
)
DECISION_STAGES = (
    "learn",
    "pre_buy",
    "already_bought",
    "holding",
    "add_or_average_down",
    "trim_or_sell",
    "compare",
    "post_event_review",
)
GURU_IDEA_TYPES = (
    "concept",
    "principle",
    "heuristic",
    "anti_pattern",
    "decision_criterion",
    "risk_frame",
    "valuation_frame",
    "time_horizon_frame",
    "behavioral_warning",
)
ANSWER_SECTION_TYPES = (
    "intent_summary",
    "guru_lens_summary",
    "principle_application",
    "business_quality_checks",
    "valuation_checks",
    "risk_checks",
    "capital_allocation_checks",
    "management_checks",
    "portfolio_context",
    "positive_signals",
    "negative_signals",
    "missing_context_questions",
    "non_advisory_decision_frame",
)
DATA_NEED_FAMILIES = (
    "future_company_metric",
    "future_company_text",
    "future_company_section",
    "portfolio_context",
    "user_context",
    "guru_corpus",
)
OBJECT_ORIGINS = (
    "source_grounded",
    "corpus_synthesized",
    "consultation_derived",
    "data_need",
    "corpus_metadata",
)
CONSULTATION_OBJECT_TYPES = (
    "idea",
    "heuristic",
    "anti_pattern",
    "decision_criterion",
    "risk_frame",
    "valuation_frame",
    "time_horizon_frame",
    "behavioral_warning",
    "investor_intent",
    "question_template",
    "question_route",
    "answer_playbook",
    "answer_section",
    "data_need",
    "clarifying_question",
    "corpus_metadata",
)


def utc_now_iso() -> str:
    """Return a stable UTC timestamp string for manifests."""
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


class GuruBaseModel(BaseModel):
    """Base model with strict fields for guru ontology contracts."""

    model_config = ConfigDict(extra="forbid")


class GuruAuthor(GuruBaseModel):
    author_key: str
    display_name: str
    organization: str
    role: str = "investor"
    corpus_type: str
    source_types: list[Literal["shareholder_letter", "memo"]]
    official_index_url: str
    default_rights_policy: str = "official_link_only_no_fulltext"
    active: bool = True
    notes: str | None = None


class GuruSourceDocument(GuruBaseModel):
    source_id: str
    author_key: str
    title: str
    source_type: Literal["shareholder_letter", "memo"]
    official_url: str
    publication_date: str | None = None
    rights_policy: str = "official_link_only_no_fulltext"
    content_hash: str | None = None
    collection_status: Literal["planned", "fetched", "parsed", "skipped"] = "planned"


class GuruSourceSpan(GuruBaseModel):
    span_id: str
    source_id: str
    span_type: Literal["section", "paragraph"]
    position: int = Field(ge=0)
    section_title: str | None = None
    text_hash: str | None = None
    storage_policy: Literal["private_cache_only"] = "private_cache_only"


class GuruPrivateSpanRecord(GuruSourceSpan):
    """Private span record used as Agent SDK input.

    The text field must stay in the running workspace, not in public ontology
    artifacts.
    """

    author_key: str
    title: str
    official_url: str
    text: str
    char_count: int = Field(ge=0)


class GuruEvidenceExcerpt(GuruBaseModel):
    excerpt_id: str
    span_id: str
    excerpt_text: str | None = None
    rights_policy: str = "public_excerpt_limited"
    max_words: int = 25


class GuruConcept(GuruBaseModel):
    concept_id: str
    author_key: str
    label_ko: str
    label_en: str | None = None
    summary_ko: str
    abstraction_level: Literal["concept"] = "concept"
    confidence: Literal["low", "medium", "high"] = "medium"
    status: Literal["candidate", "reviewed", "rejected"] = "candidate"
    supporting_span_ids: list[str] = Field(default_factory=list)


class GuruPrinciple(GuruBaseModel):
    principle_id: str
    author_key: str
    concept_ids: list[str] = Field(default_factory=list)
    label_ko: str
    rule_ko: str
    reasoning_pattern: str
    confidence: Literal["low", "medium", "high"] = "medium"
    status: Literal["candidate", "reviewed", "rejected"] = "candidate"
    support_strength: Literal["single", "recurring", "strongly_recurring"] = "single"


class GuruQuestion(GuruBaseModel):
    question_id: str
    principle_id: str
    question_ko: str
    question_type: Literal["standalone_explanation", "filing_check", "portfolio_context_check"]
    answer_requires_company: bool = False


class GuruSearchTarget(GuruBaseModel):
    target_id: str
    principle_id: str
    target_type: Literal["metric", "topic", "section"]
    target_key: str
    source_system: Literal[
        "future_company_metric",
        "future_company_text",
        "future_company_section",
        "guru_corpus",
    ]
    required_when_company_question: bool = False


class GuruApplicationRule(GuruBaseModel):
    rule_id: str
    principle_id: str
    applies_well_to: list[str] = Field(default_factory=list)
    weak_for: list[str] = Field(default_factory=list)
    warning_ko: str


class GuruRelationship(GuruBaseModel):
    relationship_id: str
    from_id: str
    to_id: str
    relation_type: Literal["supports", "refines", "contrasts", "informs", "applies_to"]
    explanation_ko: str


class GuruSafetyRule(GuruBaseModel):
    rule_id: str
    author_key: str
    prohibited_pattern: str
    safe_rewrite_ko: str
    severity: Literal["warning", "error"] = "error"


class GuruIdea(GuruBaseModel):
    idea_id: str
    author_key: str
    idea_type: Literal[
        "concept",
        "principle",
        "heuristic",
        "anti_pattern",
        "decision_criterion",
        "risk_frame",
        "valuation_frame",
        "time_horizon_frame",
        "behavioral_warning",
    ]
    label_ko: str
    label_en: str | None = None
    summary_ko: str
    body_ko: str | None = None
    supporting_span_ids: list[str] = Field(default_factory=list)
    intent_families: list[str] = Field(default_factory=list)
    confidence: Literal["low", "medium", "high"] = "medium"
    status: Literal["candidate", "reviewed", "rejected"] = "candidate"


class GuruHeuristic(GuruBaseModel):
    heuristic_id: str
    author_key: str
    label_ko: str
    rule_of_thumb_ko: str
    applies_when_ko: str | None = None
    breaks_when_ko: str | None = None
    supporting_span_ids: list[str] = Field(default_factory=list)
    status: Literal["candidate", "reviewed", "rejected"] = "candidate"


class GuruAntiPattern(GuruBaseModel):
    anti_pattern_id: str
    author_key: str
    label_ko: str
    warning_ko: str
    investor_behavior_tags: list[str] = Field(default_factory=list)
    safer_reframe_ko: str | None = None
    supporting_span_ids: list[str] = Field(default_factory=list)
    status: Literal["candidate", "reviewed", "rejected"] = "candidate"


class GuruDecisionCriterion(GuruBaseModel):
    criterion_id: str
    author_key: str
    label_ko: str
    question_ko: str
    positive_signal_ko: str | None = None
    negative_signal_ko: str | None = None
    data_need_ids: list[str] = Field(default_factory=list)
    supporting_span_ids: list[str] = Field(default_factory=list)
    status: Literal["candidate", "reviewed", "rejected"] = "candidate"


class GuruRiskFrame(GuruBaseModel):
    risk_frame_id: str
    author_key: str
    label_ko: str
    risk_question_ko: str
    downside_mechanism_ko: str | None = None
    signals_to_check: list[str] = Field(default_factory=list)
    data_need_ids: list[str] = Field(default_factory=list)
    supporting_span_ids: list[str] = Field(default_factory=list)
    status: Literal["candidate", "reviewed", "rejected"] = "candidate"


class GuruValuationFrame(GuruBaseModel):
    valuation_frame_id: str
    author_key: str
    label_ko: str
    valuation_question_ko: str
    valuation_logic_ko: str | None = None
    data_need_ids: list[str] = Field(default_factory=list)
    supporting_span_ids: list[str] = Field(default_factory=list)
    status: Literal["candidate", "reviewed", "rejected"] = "candidate"


class GuruTimeHorizonFrame(GuruBaseModel):
    horizon_frame_id: str
    author_key: str
    label_ko: str
    time_horizon_ko: str
    patience_requirement_ko: str | None = None
    supporting_span_ids: list[str] = Field(default_factory=list)
    status: Literal["candidate", "reviewed", "rejected"] = "candidate"


class GuruBehavioralWarning(GuruBaseModel):
    warning_id: str
    author_key: str
    label_ko: str
    warning_ko: str
    triggered_by_intent_families: list[str] = Field(default_factory=list)
    safe_reframe_ko: str | None = None
    supporting_span_ids: list[str] = Field(default_factory=list)
    status: Literal["candidate", "reviewed", "rejected"] = "candidate"


class GuruApplicabilityMetadata(GuruBaseModel):
    """Soft ranking hints for matching ontology objects to user questions."""

    strong_for: list[str] = Field(default_factory=list)
    possible_for: list[str] = Field(default_factory=list)
    weak_for: list[str] = Field(default_factory=list)
    anti_triggers: list[str] = Field(default_factory=list)
    requires_clarification_when: list[str] = Field(default_factory=list)
    confidence: Literal["low", "medium", "high"] = "medium"


class GuruSpecificityMetadata(GuruBaseModel):
    """Whether an object is reusable principle or source-case-specific."""

    level: Literal[
        "general_principle",
        "sector_specific",
        "asset_class_specific",
        "company_case_specific",
        "document_context_specific",
    ] = "general_principle"
    source_case_ko: str | None = None
    source_case_tags: list[str] = Field(default_factory=list)
    generalization_confidence: Literal["low", "medium", "high"] = "medium"


class GuruAnswerRoleMetadata(GuruBaseModel):
    """Soft hint for how a selected object should be used in an answer."""

    default: Literal[
        "core_lens",
        "supporting_lens",
        "caution",
        "checklist",
        "data_need",
        "contrast",
        "context",
    ] = "supporting_lens"
    possible_roles: list[
        Literal[
            "core_lens",
            "supporting_lens",
            "caution",
            "checklist",
            "data_need",
            "contrast",
            "context",
        ]
    ] = Field(default_factory=list)
    confidence: Literal["low", "medium", "high"] = "medium"


class GuruInvestorIntent(GuruBaseModel):
    intent_id: str
    intent_family: Literal[
        "learn_guru_view",
        "considering_buy",
        "already_bought",
        "holding_review",
        "sell_or_trim",
        "average_down",
        "risk_check",
        "valuation_check",
        "business_quality_check",
        "moat_check",
        "management_check",
        "capital_allocation_check",
        "balance_sheet_check",
        "cyclical_risk_check",
        "portfolio_fit",
        "position_sizing",
        "compare_candidates",
        "contrarian_check",
        "red_flag_check",
        "thesis_review",
        "post_earnings_review",
        "loss_recovery",
    ]
    label_ko: str
    decision_stage: Literal[
        "learn",
        "pre_buy",
        "already_bought",
        "holding",
        "add_or_average_down",
        "trim_or_sell",
        "compare",
        "post_event_review",
    ]
    intent_tags: list[str] = Field(default_factory=list)
    trigger_examples_ko: list[str] = Field(default_factory=list)
    requires_company_data: bool = False
    requires_portfolio_data: bool = False
    requires_user_context: bool = False
    default_playbook_ids: list[str] = Field(default_factory=list)


class GuruQuestionTemplate(GuruBaseModel):
    template_id: str
    intent_id: str
    question_pattern_ko: str
    example_questions_ko: list[str] = Field(default_factory=list)
    slot_names: list[str] = Field(default_factory=list)
    answer_requires_company: bool = False
    answer_requires_portfolio: bool = False
    answer_requires_user_context: bool = False


class GuruDataNeed(GuruBaseModel):
    data_need_id: str
    need_family: Literal[
        "future_company_metric",
        "future_company_text",
        "future_company_section",
        "portfolio_context",
        "user_context",
        "guru_corpus",
    ]
    label_ko: str
    description_ko: str
    required: bool = True
    used_for_intent_families: list[str] = Field(default_factory=list)
    query_hint_ko: str | None = None
    no_live_binding: bool = True


class GuruClarifyingQuestion(GuruBaseModel):
    clarifying_question_id: str
    intent_id: str
    question_ko: str
    slot_key: str
    required: bool = False
    ask_when_ko: str | None = None


class GuruAnswerSection(GuruBaseModel):
    section_id: str
    section_type: Literal[
        "intent_summary",
        "guru_lens_summary",
        "principle_application",
        "business_quality_checks",
        "valuation_checks",
        "risk_checks",
        "capital_allocation_checks",
        "management_checks",
        "portfolio_context",
        "positive_signals",
        "negative_signals",
        "missing_context_questions",
        "non_advisory_decision_frame",
    ]
    title_ko: str
    purpose_ko: str
    order: int = Field(ge=0)
    required: bool = True


class GuruAnswerPlaybook(GuruBaseModel):
    playbook_id: str
    intent_id: str
    label_ko: str
    author_key: str | None = None
    section_ids: list[str] = Field(default_factory=list)
    data_need_ids: list[str] = Field(default_factory=list)
    clarifying_question_ids: list[str] = Field(default_factory=list)
    safety_rule_ids: list[str] = Field(default_factory=list)
    answer_style_ko: str | None = None
    non_advisory: bool = True


class GuruQuestionRoute(GuruBaseModel):
    route_id: str
    intent_id: str
    playbook_id: str
    author_keys: list[str] = Field(default_factory=list)
    lens_object_ids: list[str] = Field(default_factory=list)
    data_need_ids: list[str] = Field(default_factory=list)
    priority: int = Field(default=100, ge=0)
    route_when_ko: str | None = None


class GuruDiscoveredSourceManifest(GuruBaseModel):
    format: str = GURU_DISCOVERY_MANIFEST_FORMAT
    schema_version: str = GURU_ONTOLOGY_SCHEMA_VERSION
    generated_at: str
    root: str
    running_root: str
    index_source_ids: list[str]
    discovered_sources: list[GuruSourceDocument] = Field(default_factory=list)


class GuruRawDocument(GuruBaseModel):
    source_id: str
    author_key: str
    title: str
    source_type: Literal["shareholder_letter", "memo"]
    official_url: str
    content_type: str | None = None
    raw_path: str | None = None
    sha256: str | None = None
    byte_count: int = Field(default=0, ge=0)
    fetched_at: str | None = None
    http_status_code: int | None = None
    status: Literal["fetched", "skipped", "error"] = "fetched"
    error: str | None = None


class GuruRawManifest(GuruBaseModel):
    format: str = GURU_RAW_MANIFEST_FORMAT
    schema_version: str = GURU_ONTOLOGY_SCHEMA_VERSION
    generated_at: str
    root: str
    running_root: str
    collection_started: bool = True
    source_count: int
    raw_documents: list[GuruRawDocument] = Field(default_factory=list)


class GuruParsedDocument(GuruBaseModel):
    source_id: str
    author_key: str
    title: str
    source_type: Literal["shareholder_letter", "memo"]
    official_url: str
    raw_path: str | None = None
    parsed_path: str | None = None
    spans_path: str | None = None
    parser: str | None = None
    content_type: str | None = None
    text_hash: str | None = None
    char_count: int = Field(default=0, ge=0)
    span_count: int = Field(default=0, ge=0)
    parsed_at: str | None = None
    status: Literal["parsed", "skipped", "error"] = "parsed"
    error: str | None = None


class GuruParsedManifest(GuruBaseModel):
    format: str = GURU_PARSED_MANIFEST_FORMAT
    schema_version: str = GURU_ONTOLOGY_SCHEMA_VERSION
    generated_at: str
    root: str
    running_root: str
    parsed_documents: list[GuruParsedDocument] = Field(default_factory=list)


class GuruOntologyCandidate(GuruBaseModel):
    """Flexible object proposed by the Agent SDK from the guru corpus.

    The object_type gives the storage family, while label/body fields are
    induced from source text instead of being pre-enumerated in code.
    """

    candidate_id: str
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    object_type: Literal[
        "concept",
        "principle",
        "question",
        "search_target",
        "application_rule",
        "relationship",
        "safety_rule",
        "idea",
        "heuristic",
        "anti_pattern",
        "decision_criterion",
        "risk_frame",
        "valuation_frame",
        "time_horizon_frame",
        "behavioral_warning",
        "investor_intent",
        "question_template",
        "question_route",
        "answer_playbook",
        "answer_section",
        "data_need",
        "clarifying_question",
        "corpus_metadata",
    ]
    object_origin: Literal[
        "source_grounded",
        "corpus_synthesized",
        "consultation_derived",
        "data_need",
        "corpus_metadata",
    ] = "source_grounded"
    label_ko: str
    label_en: str | None = None
    summary_ko: str
    body_ko: str | None = None
    supporting_span_ids: list[str] = Field(default_factory=list)
    related_candidate_ids: list[str] = Field(default_factory=list)
    applies_without_ticker: bool = True
    intent_family: Literal[
        "learn_guru_view",
        "considering_buy",
        "already_bought",
        "holding_review",
        "sell_or_trim",
        "average_down",
        "risk_check",
        "valuation_check",
        "business_quality_check",
        "moat_check",
        "management_check",
        "capital_allocation_check",
        "balance_sheet_check",
        "cyclical_risk_check",
        "portfolio_fit",
        "position_sizing",
        "compare_candidates",
        "contrarian_check",
        "red_flag_check",
        "thesis_review",
        "post_earnings_review",
        "loss_recovery",
    ] | None = None
    intent_tags: list[str] = Field(default_factory=list)
    decision_stage: Literal[
        "learn",
        "pre_buy",
        "already_bought",
        "holding",
        "add_or_average_down",
        "trim_or_sell",
        "compare",
        "post_event_review",
    ] | None = None
    question_pattern_ko: str | None = None
    example_questions_ko: list[str] = Field(default_factory=list)
    answer_section_type: Literal[
        "intent_summary",
        "guru_lens_summary",
        "principle_application",
        "business_quality_checks",
        "valuation_checks",
        "risk_checks",
        "capital_allocation_checks",
        "management_checks",
        "portfolio_context",
        "positive_signals",
        "negative_signals",
        "missing_context_questions",
        "non_advisory_decision_frame",
    ] | None = None
    answer_sections: list[str] = Field(default_factory=list)
    data_need_family: Literal[
        "future_company_metric",
        "future_company_text",
        "future_company_section",
        "portfolio_context",
        "user_context",
        "guru_corpus",
    ] | None = None
    data_need_key: str | None = None
    required_context: list[str] = Field(default_factory=list)
    requires_company_data: bool = False
    requires_portfolio_data: bool = False
    requires_user_context: bool = False
    company_data_hooks: list[dict[str, str]] = Field(default_factory=list)
    applicability: GuruApplicabilityMetadata = Field(default_factory=GuruApplicabilityMetadata)
    specificity: GuruSpecificityMetadata = Field(default_factory=GuruSpecificityMetadata)
    answer_role: GuruAnswerRoleMetadata = Field(default_factory=GuruAnswerRoleMetadata)
    confidence: Literal["low", "medium", "high"] = "medium"
    status: Literal["candidate", "reviewed", "rejected"] = "candidate"


class GuruReviewedObject(GuruBaseModel):
    reviewed_id: str
    candidate_id: str
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    object_type: Literal[
        "concept",
        "principle",
        "idea",
        "heuristic",
        "anti_pattern",
        "decision_criterion",
        "risk_frame",
        "valuation_frame",
        "time_horizon_frame",
        "behavioral_warning",
        "safety_rule",
    ]
    object_origin: Literal["source_grounded", "corpus_synthesized"]
    label_ko: str
    label_en: str | None = None
    summary_ko: str
    body_ko: str | None = None
    supporting_span_ids: list[str] = Field(default_factory=list)
    related_reviewed_ids: list[str] = Field(default_factory=list)
    intent_family: str | None = None
    applicability: GuruApplicabilityMetadata = Field(default_factory=GuruApplicabilityMetadata)
    specificity: GuruSpecificityMetadata = Field(default_factory=GuruSpecificityMetadata)
    answer_role: GuruAnswerRoleMetadata = Field(default_factory=GuruAnswerRoleMetadata)
    confidence: Literal["low", "medium", "high"] = "medium"
    status: Literal["reviewed"] = "reviewed"


class GuruReviewedConsultationObject(GuruBaseModel):
    reviewed_id: str
    candidate_id: str
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    object_type: Literal[
        "investor_intent",
        "question_template",
        "question_route",
        "answer_playbook",
        "answer_section",
        "clarifying_question",
    ]
    object_origin: Literal["consultation_derived"] = "consultation_derived"
    label_ko: str
    summary_ko: str
    question_pattern_ko: str | None = None
    example_questions_ko: list[str] = Field(default_factory=list)
    intent_family: str | None = None
    intent_tags: list[str] = Field(default_factory=list)
    decision_stage: str | None = None
    answer_section_type: str | None = None
    answer_sections: list[str] = Field(default_factory=list)
    required_context: list[str] = Field(default_factory=list)
    requires_company_data: bool = False
    requires_portfolio_data: bool = False
    requires_user_context: bool = False
    supporting_span_ids: list[str] = Field(default_factory=list)
    related_reviewed_ids: list[str] = Field(default_factory=list)
    applicability: GuruApplicabilityMetadata = Field(default_factory=GuruApplicabilityMetadata)
    specificity: GuruSpecificityMetadata = Field(default_factory=GuruSpecificityMetadata)
    answer_role: GuruAnswerRoleMetadata = Field(default_factory=GuruAnswerRoleMetadata)
    confidence: Literal["low", "medium", "high"] = "medium"
    status: Literal["reviewed"] = "reviewed"


class GuruReviewedDataNeed(GuruBaseModel):
    reviewed_id: str
    candidate_id: str
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    object_origin: Literal["data_need"] = "data_need"
    label_ko: str
    summary_ko: str
    data_need_family: Literal[
        "future_company_metric",
        "future_company_text",
        "future_company_section",
        "portfolio_context",
        "user_context",
        "guru_corpus",
    ]
    data_need_key: str
    required_context: list[str] = Field(default_factory=list)
    requires_company_data: bool = False
    requires_portfolio_data: bool = False
    requires_user_context: bool = False
    company_data_hooks: list[dict[str, str]] = Field(default_factory=list)
    supporting_span_ids: list[str] = Field(default_factory=list)
    related_reviewed_ids: list[str] = Field(default_factory=list)
    applicability: GuruApplicabilityMetadata = Field(default_factory=GuruApplicabilityMetadata)
    specificity: GuruSpecificityMetadata = Field(default_factory=GuruSpecificityMetadata)
    answer_role: GuruAnswerRoleMetadata = Field(default_factory=GuruAnswerRoleMetadata)
    confidence: Literal["low", "medium", "high"] = "medium"
    status: Literal["reviewed"] = "reviewed"


class GuruReviewedCorpusMetadata(GuruBaseModel):
    reviewed_id: str
    candidate_id: str
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    object_origin: Literal["corpus_metadata"] = "corpus_metadata"
    label_ko: str
    summary_ko: str
    supporting_span_ids: list[str] = Field(default_factory=list)
    related_reviewed_ids: list[str] = Field(default_factory=list)
    status: Literal["reviewed"] = "reviewed"


class GuruReviewedRelationship(GuruBaseModel):
    relationship_id: str
    from_id: str
    to_id: str
    relation_type: Literal["supports", "refines", "contrasts", "informs", "applies_to", "requires_data"]
    explanation_ko: str
    source_candidate_id: str | None = None


class GuruResearchPackMeta(GuruBaseModel):
    pack_id: str
    guru_keys: list[Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]]
    question: str
    corpus_version: str | None = None


class GuruAnswerability(GuruBaseModel):
    direct_source_match: bool
    confidence: Literal["low", "medium", "high"]
    source_match_strength: Literal["none", "weak", "related", "direct"]
    recommended_answer_mode: Literal[
        "lens_grounded_answer",
        "lens_with_company_bridge",
        "clarify_then_answer",
        "state_ontology_gap",
    ]
    reason: str


class GuruResearchIntent(GuruBaseModel):
    family: str | None = None
    requires_company_evidence: bool = False
    ticker: str | None = None


class GuruResearchPack(GuruBaseModel):
    """Compact MCP response contract for guru-style consultation.

    This pack is a serving-layer contract. It does not expand or mutate the
    underlying company filing ontology schema.
    """

    format: str = GURU_RESEARCH_PACK_FORMAT
    research_status: Literal[
        "sufficient_lens",
        "partial_lens",
        "ontology_gap",
        "needs_clarification",
        "needs_company_evidence",
    ]
    pack_meta: GuruResearchPackMeta
    answerability: GuruAnswerability
    intent: GuruResearchIntent
    persona_profile: dict[str, Any] = Field(default_factory=dict)
    philosophy_context: dict[str, Any] = Field(default_factory=dict)
    selected_lenses: list[dict[str, Any]] = Field(default_factory=list)
    consultation_moves: list[dict[str, Any]] = Field(default_factory=list)
    data_needs: list[dict[str, Any]] = Field(default_factory=list)
    company_context: dict[str, Any] = Field(default_factory=dict)
    source_anchors: list[dict[str, Any]] = Field(default_factory=list)
    clarifying_questions: list[str] = Field(default_factory=list)
    company_bridge: dict[str, Any] = Field(default_factory=dict)
    trace_recommendations: list[dict[str, Any]] = Field(default_factory=list)
    agent_autonomy: dict[str, Any] = Field(default_factory=dict)
    do_not_call: list[str] = Field(default_factory=list)
    warnings: list[str] = Field(default_factory=list)


class GuruCompanyIdentity(GuruBaseModel):
    ticker: str | None = None
    company_name: str | None = None
    subject: str
    unresolved: bool = False


class GuruCompanyOntologyContext(GuruBaseModel):
    """Bounded runtime summary produced from company ontology reads.

    This object is not persisted into, and does not extend, the company filing
    ontology schema. It lets guru lens selection use company vocabulary without
    hard-coding ticker-to-industry mappings.
    """

    format: str = GURU_COMPANY_ONTOLOGY_CONTEXT_FORMAT
    ticker: str | None = None
    company_name: str | None = None
    source: str = "company_ontology_runtime"
    context_terms: list[str] = Field(default_factory=list)
    business_context_terms: list[str] = Field(default_factory=list)
    risk_context_terms: list[str] = Field(default_factory=list)
    available_company_topics: list[str] = Field(default_factory=list)
    context_tags: list[str] = Field(default_factory=list)
    confidence: Literal["low", "medium", "high"] | None = None
    source_payload_keys: list[str] = Field(default_factory=list)


class GuruCompanyContextAnchor(GuruBaseModel):
    """A trusted, neutral fact used to ground an investigation question."""

    anchor_id: str
    kind: Literal[
        "business_description",
        "primary_activity",
        "product",
        "segment",
        "revenue_logic",
        "sector",
    ]
    text: str


class GuruLightCompanyContext(GuruBaseModel):
    """Company identity and neutral business context available before research.

    This is deliberately distinct from filing retrieval output: it must not
    carry recent performance, investment conclusions, or valuation data.
    """

    format: str = GURU_LIGHT_COMPANY_CONTEXT_FORMAT
    ticker: str
    company_name: str
    sector: str | None = None
    industry: str | None = None
    business_description: str
    primary_activities: list[str] = Field(default_factory=list)
    products_or_segments: list[str] = Field(default_factory=list)
    revenue_logic: str | None = None
    context_anchors: list[GuruCompanyContextAnchor] = Field(default_factory=list)
    filing_availability: dict[str, bool] = Field(default_factory=dict)


class GuruInvestigationQuestionDraft(GuruBaseModel):
    """A philosophy-grounded research question authored by the main agent."""

    question: str
    guru_principle_ids: list[str]
    company_context_anchor_ids: list[str]
    hypothesis: str
    counter_hypothesis: str
    evidence_needed: list[str]
    strengthens_if: str
    weakens_if: str
    why_material: str
    # The main agent assigns the role. The server only checks that the
    # question set has one central tension; it never authors a question.
    decision_role: Literal[
        "main_tension",
        "supporting_evidence",
        "countercase",
        "change_condition",
    ] | None = None


class GuruInvestigationQuestion(GuruInvestigationQuestionDraft):
    """A server-sealed investigation question with a deterministic id."""

    question_id: str


class GuruInvestigationBrief(GuruBaseModel):
    """Bounded handoff from the main Guru to filing evidence retrieval."""

    format: str = GURU_INVESTIGATION_BRIEF_FORMAT
    brief_hash: str
    research_pack_id: str
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    ticker: str
    company_context_hash: str
    questions: list[GuruInvestigationQuestion]


class GuruEvidenceAssessment(GuruBaseModel):
    """The main Guru's internal, evidence-bound analysis of one question."""

    question_id: str
    evidence_object_ids: list[str] = Field(default_factory=list)
    verdict: Literal["supported", "mixed", "unresolved"]
    reasoning: str


class GuruCompanyResearchContext(GuruBaseModel):
    """Runtime-built filing context for the default Guru company path.

    This intentionally carries only evidence returned by the company MCP.  It
    is not a verifier output and therefore cannot authorize a ``supported``
    verdict by itself.
    """

    format: Literal["krw-guru-company-research-context/v1"]
    ticker: str
    brief_hash: str
    question_ids: list[str] = Field(min_length=1)
    evidence_units: list[dict[str, Any]] = Field(min_length=1)
    source_object_ids: list[str] = Field(min_length=1)
    research_status: Literal["evidence_found", "partial"] = "partial"


class GuruAgentEvidenceAnalysis(GuruBaseModel):
    """Private main-agent interpretation; never render this object directly."""

    assessments: list[GuruEvidenceAssessment]
    overall_judgment: str


class GuruValidatedEvidenceAnalysis(GuruBaseModel):
    """Result returned after server validation of a main-agent analysis."""

    format: str = GURU_VALIDATED_EVIDENCE_ANALYSIS_FORMAT
    brief_hash: str
    evidence_mode: Literal["contextual"] = "contextual"
    research_context_hash: str
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    ticker: str
    agent_analysis: GuruAgentEvidenceAnalysis
    decision_frame: dict[str, Any] = Field(default_factory=dict)
    validation: dict[str, Any] = Field(default_factory=dict)


class GuruDynamicQuestionPlanItem(GuruBaseModel):
    """One guru-specific, company-specific question for filing evidence retrieval."""

    question_id: str
    question_en: str
    question_ko_label: str
    why_guru_relevant: str
    why_company_specific: str
    evidence_needed: list[str] = Field(default_factory=list)
    priority: Literal["highest", "high", "medium"] = "medium"
    answer_role: Literal[
        "main_tension",
        "supporting_evidence",
        "counterweight",
        "change_condition",
        "do_not_overstate",
    ] = "supporting_evidence"
    retrieval_query_en: str
    stop_condition: str
    do_not_overstate: str


class GuruDynamicQuestionPlan(GuruBaseModel):
    """Question plan produced by the guru lens before company evidence retrieval."""

    format: str = GURU_DYNAMIC_QUESTION_PLAN_FORMAT
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    ticker: str | None = None
    company_name: str | None = None
    subject: str
    user_intent: str | None = None
    company_context_terms: list[str] = Field(default_factory=list)
    questions: list[GuruDynamicQuestionPlanItem] = Field(default_factory=list)
    construction_rules: list[str] = Field(default_factory=list)


class GuruCompanyFilingBrief(GuruBaseModel):
    """Serving-layer bridge from guru lenses to company filing research."""

    format: str = GURU_COMPANY_FILING_BRIEF_FORMAT
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    original_question: str
    company_identity: GuruCompanyIdentity
    requires_company_evidence: bool
    requires_identifier_clarification: bool = False
    company_context: dict[str, Any] = Field(default_factory=dict)
    company_research_question_ko: str
    company_research_question_en: str
    query_terms: list[str] = Field(default_factory=list)
    required_filing_topics: list[str] = Field(default_factory=list)
    candidate_filing_topics: list[str] = Field(default_factory=list)
    filtered_out_topics: list[dict[str, str]] = Field(default_factory=list)
    lens_specific_evidence_requests: list[str] = Field(default_factory=list)
    generic_filing_requirements: list[str] = Field(default_factory=list)
    data_need_keys: list[str] = Field(default_factory=list)
    dynamic_question_plan: GuruDynamicQuestionPlan | None = None
    investigation_brief: GuruInvestigationBrief | None = None
    missing_inputs: list[str] = Field(default_factory=list)
    recommended_company_mcp_call: dict[str, Any] = Field(default_factory=dict)
    boundary: str = (
        "Guru filing brief translates selected guru ontology needs into company filing "
        "research inputs. It does not supply company facts."
    )


class GuruCompanyEvidenceAlignment(GuruBaseModel):
    lens_id: str | None = None
    lens_label_ko: str | None = None
    required_company_evidence: list[str] = Field(default_factory=list)
    status: Literal["supported", "partial", "missing", "pending_company_research", "not_required"]
    company_fact_refs: list[dict[str, Any]] = Field(default_factory=list)
    reason_ko: str


class GuruCompanyResearchPack(GuruBaseModel):
    """Internal answer-prep pack joining guru lenses with optional company facts.

    This is a serving-layer object. Company evidence stays opaque so the Guru
    module does not expand the KRW company ontology schema.
    """

    format: str = GURU_COMPANY_RESEARCH_PACK_FORMAT
    user_question: str
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    company_identity: GuruCompanyIdentity
    guru_pack: dict[str, Any]
    company_context: dict[str, Any] = Field(default_factory=dict)
    company_filing_brief: dict[str, Any]
    company_evidence_pack: dict[str, Any] = Field(default_factory=dict)
    evidence_alignment: list[GuruCompanyEvidenceAlignment] = Field(default_factory=list)
    missing_evidence: list[str] = Field(default_factory=list)
    judgment_conditions: dict[str, Any] = Field(default_factory=dict)
    render_hints: dict[str, Any] = Field(default_factory=dict)
    answer_contract: dict[str, Any] = Field(default_factory=dict)
    boundaries: list[str] = Field(default_factory=list)


class GuruCompanyEvidenceReview(GuruBaseModel):
    """Internal interpretation guide after company evidence is available.

    This object keeps final prose flexible. It reviews how filing evidence
    supports, weakens, or limits the selected guru lenses, but it does not
    write the user-facing answer or impose a fixed report structure.
    """

    format: str = GURU_COMPANY_EVIDENCE_REVIEW_FORMAT
    user_question: str
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    company_identity: GuruCompanyIdentity
    lens_alignment: Literal["strengthens", "mixed", "weakens", "unresolved"] = "unresolved"
    primary_interpretation_ko: str
    advisor_question_ko: str | None = None
    strengthened_by: list[str] = Field(default_factory=list)
    weakened_by: list[str] = Field(default_factory=list)
    what_to_emphasize: list[str] = Field(default_factory=list)
    what_not_to_overstate: list[str] = Field(default_factory=list)
    change_conditions: list[str] = Field(default_factory=list)
    missing_evidence: list[str] = Field(default_factory=list)
    evidence_alignment: list[GuruCompanyEvidenceAlignment] = Field(default_factory=list)
    answer_contract: dict[str, Any] = Field(default_factory=dict)
    boundaries: list[str] = Field(default_factory=list)


class GuruAnswerRenderPlan(GuruBaseModel):
    """Stable rendering plan for final advisor prose.

    The plan gives structure and voice hints, not new investment principles.
    """

    format: str = GURU_ANSWER_RENDER_PLAN_FORMAT
    author_key: Literal["buffett", "marks", "ackman", "flatt", "terry_smith"]
    opening_style: str
    first_question: str
    reframe: str
    supported_points: list[str] = Field(default_factory=list)
    concerns: list[str] = Field(default_factory=list)
    missing_evidence: list[str] = Field(default_factory=list)
    judgment_conditions: dict[str, Any] = Field(default_factory=dict)
    next_question: str | None = None
    forbidden_output_patterns: list[str] = Field(default_factory=list)
    source_fields: list[str] = Field(default_factory=list)


class GuruRejectedCandidate(GuruBaseModel):
    candidate_id: str
    author_key: str | None = None
    object_type: str | None = None
    object_origin: str | None = None
    label_ko: str | None = None
    rejection_stage: str
    rejection_reason: str
    source_payload: dict[str, Any] = Field(default_factory=dict)


class GuruCurationReport(GuruBaseModel):
    format: str = GURU_CURATION_REPORT_FORMAT
    schema_version: str = GURU_ONTOLOGY_SCHEMA_VERSION
    generated_at: str
    root: str
    running_root: str
    input_candidates: int
    accepted_guru_objects: int
    accepted_consultation_objects: int
    accepted_data_needs: int
    accepted_corpus_metadata: int
    relationships: int
    rejected_candidates: int
    warnings: list[str] = Field(default_factory=list)
    rejection_reasons: dict[str, int] = Field(default_factory=dict)
    files: dict[str, str] = Field(default_factory=dict)


class GuruExtractionBatch(GuruBaseModel):
    batch_id: str
    span_ids: list[str]
    source_ids: list[str]
    prompt_path: str
    output_path: str
    status: Literal["planned", "completed", "error"] = "planned"
    agent_sdk_called: bool = False
    error: str | None = None


class GuruExtractionManifest(GuruBaseModel):
    format: str = GURU_EXTRACTION_MANIFEST_FORMAT
    schema_version: str = GURU_ONTOLOGY_SCHEMA_VERSION
    generated_at: str
    root: str
    running_root: str
    execution_mode: Literal["dry_run", "agent_sdk"] = "dry_run"
    agent_sdk_called: bool = False
    model: str | None = None
    concurrency: int = Field(default=1, ge=1)
    extraction_started: bool = False
    batches: list[GuruExtractionBatch] = Field(default_factory=list)
    output_files: dict[str, str] = Field(default_factory=dict)


class GuruOntologyManifest(GuruBaseModel):
    format: str = GURU_ONTOLOGY_MANIFEST_FORMAT
    schema_version: str = GURU_ONTOLOGY_SCHEMA_VERSION
    generated_at: str
    collection_started: bool = False
    extraction_started: bool = False
    author_keys: list[str]
    object_types: list[str]
    notes: list[str] = Field(default_factory=list)


class GuruSourceManifest(GuruBaseModel):
    format: str = GURU_SOURCE_MANIFEST_FORMAT
    schema_version: str = GURU_ONTOLOGY_SCHEMA_VERSION
    generated_at: str
    collection_started: bool = False
    authors: list[GuruAuthor]
    planned_sources: list[GuruSourceDocument] = Field(default_factory=list)
    seed_sources: list[GuruSourceDocument] = Field(default_factory=list)
    source_policy: dict[str, Any] = Field(default_factory=dict)


class GuruPipelineStage(GuruBaseModel):
    stage: str
    status: Literal["planned", "disabled", "pending"]
    writes_full_text: bool = False
    notes: str | None = None


class GuruCollectionPlan(GuruBaseModel):
    format: str = GURU_COLLECTION_PLAN_FORMAT
    schema_version: str = GURU_ONTOLOGY_SCHEMA_VERSION
    generated_at: str
    author_keys: list[str]
    collection_started: bool = False
    extraction_started: bool = False
    ticker_required: bool = False
    existing_ontology_schema_changed: bool = False
    mcp_changed: bool = False
    skills_changed: bool = False
    workspace: dict[str, str]
    stages: list[GuruPipelineStage]
    ontology_object_types: list[str]
    existing_ontology_mapping: list[dict[str, str]]
    next_commands: list[str]
