"""Fast read-only guru lens selection for investor consultation questions."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Mapping, Sequence

from krw_capability_runtime.guru import mcp_tools
from krw_capability_runtime.guru.company_context import coerce_company_context, company_context_topics


DEFAULT_LENS_LIMIT = 5
DEFAULT_DATA_NEED_LIMIT = 6
LENS_SELECTION_VERSION = "krw-guru-lens-selection/v1"


def select_guru_lenses(
    *,
    question: str,
    root: str | Path | None = None,
    author_keys: Sequence[str] | None = None,
    ticker: str | None = None,
    company_context: Mapping[str, Any] | None = None,
    intent_family: str | None = None,
    limit: int = DEFAULT_LENS_LIMIT,
    data_need_limit: int = DEFAULT_DATA_NEED_LIMIT,
) -> dict[str, Any]:
    """Select the most relevant guru ontology lenses before answer generation.

    This is intentionally narrower than ``guru_query_context_tool``. It does not
    compose an answer and does not retrieve company facts; it only selects
    ontology-derived lenses and the evidence hooks a later filing-research step
    would need.
    """
    root_path = mcp_tools._resolve_root(root)
    selected_authors = mcp_tools._selected_author_keys(question, author_keys)
    intent_families = _intent_families(question, intent_family)
    inferred_intent = intent_families[0] if intent_families else None
    company_context_model = coerce_company_context(company_context or None, ticker=ticker)
    lens_limit = mcp_tools._limit(limit, maximum=20)
    need_limit = mcp_tools._limit(data_need_limit, maximum=30)
    scoring_question = mcp_tools._scoring_query(question, ticker, company_context_model)
    bundle = mcp_tools._load_reviewed_bundle(
        root_path,
        author_keys=selected_authors,
        query=scoring_question,
        family_limits={
            "guru_objects": mcp_tools._fts_shadow_limit(lens_limit),
            "data_needs": mcp_tools._fts_shadow_limit(need_limit),
        },
    )
    company_context_payload = (
        company_context_model.model_dump(mode="json", exclude_none=True)
        if company_context_model is not None
        else {}
    )

    lens_rows = _select_lens_portfolio(
        query=scoring_question,
        bundle=bundle,
        author_keys=selected_authors,
        intent_families=intent_families,
        limit=lens_limit,
    )
    mcp_tools._hydrate_candidate_payloads(root_path, bundle, lens_rows)
    mcp_tools._hydrate_candidate_related_objects(root_path, bundle, lens_rows)
    data_need_rows = _select_related_data_needs(
        question=scoring_question,
        bundle=bundle,
        lens_rows=lens_rows,
        author_keys=selected_authors,
        intent_family=inferred_intent,
        limit=need_limit,
    )
    mcp_tools._hydrate_candidate_payloads(root_path, bundle, data_need_rows)
    mcp_tools._hydrate_candidate_relationships(
        root_path,
        bundle,
        [*lens_rows, *data_need_rows],
    )
    requires_company_evidence = _requires_company_evidence(
        question=question,
        ticker=ticker,
    )
    requires_identifier_clarification = _requires_identifier_clarification(
        question=question,
        ticker=ticker,
        requires_company_evidence=requires_company_evidence,
    )
    clarifying_questions = mcp_tools._clarifying_questions_for_question(
        question,
        requires_company_evidence,
        ticker=ticker,
    )
    evidence_requirements = _evidence_requirements(
        question=question,
        intent_family=inferred_intent,
        data_need_rows=data_need_rows,
        company_context=company_context_model,
    )
    lens_payloads = [
        _lens_payload(
            question=scoring_question,
            row=row,
            bundle=bundle,
            intent_family=inferred_intent,
            related_data_needs=_data_needs_for_lens(row, data_need_rows, bundle),
            requires_company_evidence=requires_company_evidence,
            lens_role=_lens_role(row, scoring_question, inferred_intent),
            intent_families=intent_families,
        )
        for row in lens_rows
    ]
    status = _selection_status(
        has_lenses=bool(lens_payloads),
        requires_company_evidence=requires_company_evidence,
        clarifying_questions=clarifying_questions,
        requires_identifier_clarification=requires_identifier_clarification,
    )
    answer_evidence_plan = _answer_evidence_plan(
        question=question,
        intent_family=inferred_intent,
        intent_families=intent_families,
        requires_company_evidence=requires_company_evidence,
        requires_identifier_clarification=requires_identifier_clarification,
        evidence_requirements=evidence_requirements,
        data_need_rows=data_need_rows,
    )
    return {
        "lens_selection_version": LENS_SELECTION_VERSION,
        "selection_status": status,
        "question": question,
        "ticker": mcp_tools._clean_optional(ticker),
        "root": str(root_path),
        "runtime": mcp_tools._bundle_runtime(bundle),
        "selected_author_keys": selected_authors,
        "selected_authors": [
            {"author_key": key, "display_name": mcp_tools.AUTHOR_DISPLAY_NAMES.get(key, key)}
            for key in selected_authors
        ],
        "intent_family": inferred_intent,
        "primary_intent": inferred_intent,
        "secondary_intents": intent_families[1:],
        "intent_families": intent_families,
        "requires_company_evidence": requires_company_evidence,
        "requires_identifier_clarification": requires_identifier_clarification,
        "company_context": company_context_payload,
        "clarifying_questions": clarifying_questions,
        "count": len(lens_payloads),
        "selected_lenses": lens_payloads,
        "lens_roles": _lens_role_summary(lens_payloads),
        "data_needs": [
            mcp_tools._compact_result(
                row,
                score=mcp_tools._context_score(
                    scoring_question,
                    row,
                    intent_family=inferred_intent,
                    row_family="data_need",
                ),
                relationships=bundle["relationships_by_id"],
                include_relationships=False,
            )
            for row in data_need_rows
        ],
        "company_bridge": _company_bridge(
            question=question,
            ticker=ticker,
            requires_company_evidence=requires_company_evidence,
            requires_identifier_clarification=requires_identifier_clarification,
            evidence_requirements=evidence_requirements,
            data_need_rows=data_need_rows,
            company_context=company_context_payload,
        ),
        "answer_evidence_plan": answer_evidence_plan,
        "usage": {
            "purpose": (
                "Fast guru principle selection only. This tool does not create company "
                "facts, does not mutate ontology, and does not produce final advice."
            ),
            "next_step": (
                "If requires_company_evidence is true, pass company_bridge requirements to "
                "existing KRW Ontology filing research before giving a company-specific view."
            ),
            "boundary": (
                "Guru ontology supplies lenses and evidence questions; company facts must "
                "come from filing research."
            ),
        },
    }


def guru_select_lenses_tool(
    *,
    question: str,
    root: str | Path | None = None,
    author_keys: Sequence[str] | None = None,
    ticker: str | None = None,
    company_context: Mapping[str, Any] | None = None,
    company_context_json: Any = None,
    intent_family: str | None = None,
    limit: int = DEFAULT_LENS_LIMIT,
    data_need_limit: int = DEFAULT_DATA_NEED_LIMIT,
    response_format: mcp_tools.GuruResponseFormat = mcp_tools.GuruResponseFormat.JSON,
) -> str:
    """Return selected guru lenses and company-evidence requirements."""
    payload = select_guru_lenses(
        question=question,
        root=root,
        author_keys=author_keys,
        ticker=ticker,
        company_context=mcp_tools._json_arg(
            company_context if company_context is not None else company_context_json,
            field_name="company_context",
        ),
        intent_family=intent_family,
        limit=limit,
        data_need_limit=data_need_limit,
    )
    if response_format == mcp_tools.GuruResponseFormat.MARKDOWN:
        return _selection_markdown(payload)
    return mcp_tools._json(payload)


def _intent_families(question: str, intent_family: str | None) -> list[str]:
    explicit = mcp_tools._clean_optional(intent_family)
    inferred = mcp_tools._infer_intent_families(question)
    if explicit:
        return [explicit, *[intent for intent in inferred if intent != explicit]]
    return inferred


def _select_lens_portfolio(
    *,
    query: str,
    bundle: Mapping[str, Any],
    author_keys: Sequence[str],
    intent_families: Sequence[str],
    limit: int,
) -> list[dict[str, Any]]:
    candidates = mcp_tools._filter_rows(bundle["guru_objects"], author_keys=author_keys)
    scored = [
        (
            _multi_intent_context_score(
                query,
                row,
                intent_families=intent_families,
                row_family="guru_object",
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
    ranked = [row for score, row in scored if score > 0]
    selected: list[dict[str, Any]] = []
    selected_ids: set[str] = set()

    if len(author_keys) > 1:
        for author_key in author_keys:
            for row in ranked:
                if row.get("author_key") != author_key:
                    continue
                if _append_unique_lens(selected, selected_ids, row, limit):
                    break
            if len(selected) >= limit:
                return selected

    for role in _preferred_lens_roles(intent_families):
        for row in ranked:
            if _lens_role(row, query, intent_families[0] if intent_families else None) != role:
                continue
            if _append_unique_lens(selected, selected_ids, row, limit):
                break
        if len(selected) >= limit:
            return selected

    for row in ranked:
        if _append_unique_lens(selected, selected_ids, row, limit):
            if len(selected) >= limit:
                break
    return selected


def _append_unique_lens(
    selected: list[dict[str, Any]],
    selected_ids: set[str],
    row: dict[str, Any],
    limit: int,
) -> bool:
    reviewed_id = str(row.get("reviewed_id") or id(row))
    if reviewed_id in selected_ids or len(selected) >= limit:
        return False
    selected.append(row)
    selected_ids.add(reviewed_id)
    return True


def _multi_intent_context_score(
    query: str,
    row: Mapping[str, Any],
    *,
    intent_families: Sequence[str],
    row_family: str,
) -> int:
    if not intent_families:
        return mcp_tools._context_score(query, row, intent_family=None, row_family=row_family)
    scores = [
        mcp_tools._context_score(query, row, intent_family=intent, row_family=row_family)
        for intent in intent_families
    ]
    positive_matches = sum(1 for score in scores if score > 0)
    return max(scores) + min(positive_matches * 2, 6)


def _preferred_lens_roles(intent_families: Sequence[str]) -> list[str]:
    intents = set(intent_families)
    if "position_sizing" in intents:
        return ["portfolio_lens", "risk_lens", "core_lens", "checklist_lens", "counter_lens"]
    if "sell_or_trim" in intents:
        return ["risk_lens", "counter_lens", "portfolio_lens", "core_lens", "checklist_lens"]
    if "valuation_check" in intents:
        return ["core_lens", "counter_lens", "risk_lens", "checklist_lens", "supporting_lens"]
    if intents & {"risk_check", "cyclical_risk_check", "contrarian_check"}:
        return ["risk_lens", "counter_lens", "core_lens", "checklist_lens", "supporting_lens"]
    if intents & {"holding_review", "business_quality_check", "capital_allocation_check"}:
        return ["core_lens", "supporting_lens", "checklist_lens"]
    return ["core_lens", "checklist_lens", "risk_lens", "counter_lens", "supporting_lens"]


def _lens_role(row: Mapping[str, Any], question: str, intent_family: str | None) -> str:
    text = mcp_tools._row_text(row)
    object_type = str(row.get("object_type") or "")
    answer_role = mcp_tools._resolved_answer_role(row)
    role_values = [
        str(answer_role.get("default") or ""),
        *[str(value) for value in mcp_tools._list_value(answer_role.get("possible_roles"))],
    ]
    if intent_family == "position_sizing" and mcp_tools._contains_any(
        text,
        ("포트폴리오", "비중", "집중", "분산", "position", "concentration", "현금"),
    ):
        return "portfolio_lens"
    if object_type in {"risk_frame", "behavioral_warning"} or "caution" in role_values:
        return "risk_lens"
    if object_type == "anti_pattern" or "contrast" in role_values:
        return "counter_lens"
    if "checklist" in role_values or object_type in {"question_template", "answer_playbook"}:
        return "checklist_lens"
    if "core_lens" in role_values or object_type in {"principle", "decision_criterion"}:
        return "core_lens"
    return "supporting_lens"


def _matched_intents(row: Mapping[str, Any], intent_families: Sequence[str]) -> list[str]:
    matched: list[str] = []
    row_intent = row.get("intent_family")
    applicability = mcp_tools._mapping_value(row.get("applicability"))
    strong_for = {str(value) for value in mcp_tools._list_value(applicability.get("strong_for"))}
    possible_for = {
        str(value) for value in mcp_tools._list_value(applicability.get("possible_for"))
    }
    for intent in intent_families:
        if intent == row_intent or intent in strong_for or intent in possible_for:
            matched.append(intent)
    return matched


def _lens_role_summary(lenses: Sequence[Mapping[str, Any]]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for lens in lenses:
        role = str(lens.get("lens_role") or "supporting_lens")
        counts[role] = counts.get(role, 0) + 1
    return dict(sorted(counts.items()))


def _select_related_data_needs(
    *,
    question: str,
    bundle: Mapping[str, Any],
    lens_rows: Sequence[Mapping[str, Any]],
    author_keys: Sequence[str],
    intent_family: str | None,
    limit: int,
) -> list[dict[str, Any]]:
    direct_rows = _direct_data_need_rows(bundle, lens_rows)
    ranked_rows = mcp_tools._rank_context_rows(
        query=question,
        rows=bundle["data_needs"],
        author_keys=author_keys,
        intent_family=intent_family,
        row_family="data_need",
        limit=limit,
    )
    direct_ids = {str(row.get("reviewed_id") or "") for row in direct_rows}
    rows = _dedupe_rows([*direct_rows, *ranked_rows])
    rows = [
        row
        for row in rows
        if _data_need_selection_score(
            question=question,
            row=row,
            intent_family=intent_family,
            is_direct=str(row.get("reviewed_id") or "") in direct_ids,
        )
        >= 8
        or str(row.get("reviewed_id") or "") in direct_ids
    ]
    rows.sort(
        key=lambda row: (
            -_data_need_selection_score(
                question=question,
                row=row,
                intent_family=intent_family,
                is_direct=str(row.get("reviewed_id") or "") in direct_ids,
            ),
            row.get("author_key") or "",
            row.get("reviewed_id") or "",
        )
    )
    rows = mcp_tools._prioritize_company_data_needs(rows, limit)
    return rows[:limit]


def _direct_data_need_rows(
    bundle: Mapping[str, Any],
    lens_rows: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    objects_by_id = bundle["objects_by_id"]
    relationships_by_id = bundle["relationships_by_id"]
    rows: list[dict[str, Any]] = []
    for lens in lens_rows:
        for related_id in lens.get("related_reviewed_ids") or []:
            row = objects_by_id.get(str(related_id))
            if row and mcp_tools._row_family(row) == "data_need":
                rows.append(row)
        reviewed_id = str(lens.get("reviewed_id") or "")
        for relationship in relationships_by_id.get(reviewed_id, []):
            other_id = (
                relationship.get("to_id")
                if relationship.get("from_id") == reviewed_id
                else relationship.get("from_id")
            )
            if not other_id:
                continue
            row = objects_by_id.get(str(other_id))
            if row and mcp_tools._row_family(row) == "data_need":
                rows.append(row)
    return _dedupe_rows(rows)


def _data_needs_for_lens(
    lens: Mapping[str, Any],
    data_need_rows: Sequence[dict[str, Any]],
    bundle: Mapping[str, Any],
) -> list[dict[str, Any]]:
    related_ids = {str(value) for value in lens.get("related_reviewed_ids") or []}
    reviewed_id = str(lens.get("reviewed_id") or "")
    for relationship in bundle["relationships_by_id"].get(reviewed_id, []):
        other_id = (
            relationship.get("to_id")
            if relationship.get("from_id") == reviewed_id
            else relationship.get("from_id")
        )
        if other_id:
            related_ids.add(str(other_id))
    rows = [
        row
        for row in data_need_rows
        if str(row.get("reviewed_id") or "") in related_ids
        or reviewed_id in {str(value) for value in row.get("related_reviewed_ids") or []}
    ]
    return rows or list(data_need_rows[:2])


def _lens_payload(
    *,
    question: str,
    row: Mapping[str, Any],
    bundle: Mapping[str, Any],
    intent_family: str | None,
    related_data_needs: Sequence[dict[str, Any]],
    requires_company_evidence: bool,
    lens_role: str,
    intent_families: Sequence[str],
) -> dict[str, Any]:
    score = mcp_tools._context_score(
        question,
        row,
        intent_family=intent_family,
        row_family="guru_object",
    )
    result = mcp_tools._compact_result(
        row,
        score=score,
        relationships=bundle["relationships_by_id"],
        include_relationships=False,
    )
    evidence = _evidence_requirements(
        question=question,
        intent_family=intent_family,
        data_need_rows=related_data_needs,
    )
    result.update(
        {
            "lens_role": lens_role,
            "matched_intents": _matched_intents(row, intent_families),
            "why_selected": _why_selected(row, intent_family, requires_company_evidence),
            "related_data_needs": [
                mcp_tools._compact_result(
                    need,
                    score=mcp_tools._context_score(
                        question,
                        need,
                        intent_family=intent_family,
                        row_family="data_need",
                    ),
                    relationships=bundle["relationships_by_id"],
                    include_relationships=False,
                )
                for need in related_data_needs
            ],
            "company_evidence_requirements": evidence if requires_company_evidence else [],
            "conditional_company_evidence_requirements": (
                [] if requires_company_evidence else evidence
            ),
        }
    )
    return result


def _why_selected(
    row: Mapping[str, Any],
    intent_family: str | None,
    requires_company_evidence: bool,
) -> str:
    parts: list[str] = []
    label = row.get("label_ko") or row.get("label_en") or row.get("reviewed_id")
    role = mcp_tools._resolved_answer_role(row).get("default")
    applicability = mcp_tools._mapping_value(row.get("applicability"))
    strong_for = [str(value) for value in mcp_tools._list_value(applicability.get("strong_for"))]
    if intent_family and intent_family in strong_for:
        parts.append(f"질문 의도({intent_family})에 strong_for로 연결되어 있습니다.")
    elif intent_family and row.get("intent_family") == intent_family:
        parts.append(f"질문 의도({intent_family})와 같은 intent_family입니다.")
    else:
        parts.append("질문 문맥과 온톨로지 텍스트/메타데이터가 의미적으로 맞습니다.")
    if role:
        parts.append(f"상담 답변에서는 {role} 역할로 쓰기 적합합니다.")
    if requires_company_evidence:
        parts.append("종목 판단에는 별도 공시 근거가 필요합니다.")
    return f"{label}: " + " ".join(parts)


def _requires_company_evidence(*, question: str, ticker: str | None) -> bool:
    if mcp_tools._question_has_unresolved_asset_wrapper_ambiguity(question, ticker):
        return False
    explicit_no_company = (
        mcp_tools._question_denies_company_subject(question)
        or mcp_tools._question_is_generic_lens_framework(question)
    ) and not mcp_tools._clean_optional(ticker)
    if explicit_no_company:
        return False
    if mcp_tools._clean_optional(ticker):
        return True
    return mcp_tools._question_mentions_company_need(question)


def _requires_identifier_clarification(
    *,
    question: str,
    ticker: str | None,
    requires_company_evidence: bool,
) -> bool:
    return (
        requires_company_evidence
        and not mcp_tools._clean_optional(ticker)
        and not mcp_tools._has_ticker_like_token(question)
    )


def _evidence_requirements(
    *,
    question: str,
    intent_family: str | None,
    data_need_rows: Sequence[Mapping[str, Any]],
    company_context: Mapping[str, Any] | None = None,
) -> list[str]:
    requirements = mcp_tools._generic_filing_requirements(question, intent_family)
    for row in data_need_rows:
        requirements.extend(
            _normalized_company_hook(value) for value in row.get("company_data_hooks") or []
        )
        if row.get("data_need_key"):
            requirements.append(str(row["data_need_key"]))
    requirements.extend(company_context_topics(company_context))
    return list(dict.fromkeys(value for value in requirements if value))


def _data_need_selection_score(
    *,
    question: str,
    row: Mapping[str, Any],
    intent_family: str | None,
    is_direct: bool,
) -> int:
    score = mcp_tools._context_score(
        question,
        row,
        intent_family=intent_family,
        row_family="data_need",
    )
    if is_direct:
        score += 10
    if row.get("requires_company_data") or row.get("company_data_hooks"):
        score += 3
    return score


def _normalized_company_hook(value: Any) -> str:
    if isinstance(value, str):
        return value.strip()
    if isinstance(value, Mapping):
        for key in ("section", "metric", "field", "table", "data_key", "key"):
            nested = value.get(key)
            if isinstance(nested, str) and nested.strip():
                return nested.strip()
        description = value.get("description")
        if isinstance(description, str):
            return description.strip()
    return ""


def _company_bridge(
    *,
    question: str,
    ticker: str | None,
    requires_company_evidence: bool,
    requires_identifier_clarification: bool,
    evidence_requirements: Sequence[str],
    data_need_rows: Sequence[Mapping[str, Any]],
    company_context: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    clean_ticker = mcp_tools._clean_optional(ticker)
    hook_text = ", ".join(evidence_requirements[:12])
    subject = clean_ticker or "the selected company"
    company_context_payload = dict(company_context or {})
    if requires_identifier_clarification:
        return {
            "use_existing_krw_ontology_mcp": False,
            "ticker": clean_ticker,
            "requires_identifier_clarification": True,
            "filing_evidence_requirements": [],
            "conditional_filing_evidence_requirements": list(evidence_requirements),
            "data_need_keys": _data_need_keys(data_need_rows),
            "company_context": company_context_payload,
            "next_step": (
                "Resolve the exact company identifier, ticker, exchange, and share class before "
                "calling KRW Ontology filing research."
            ),
            "brief_en": (
                "A company appears to be selected, but the identifier is unresolved. "
                f"After resolution, retrieve filing evidence for: {hook_text}. "
                f"User question: {question}"
            ),
            "boundary": "Guru lens selection is separate from company identity resolution.",
        }
    if requires_company_evidence:
        return {
            "use_existing_krw_ontology_mcp": True,
            "ticker": clean_ticker,
            "requires_identifier_clarification": False,
            "filing_evidence_requirements": list(evidence_requirements),
            "conditional_filing_evidence_requirements": [],
            "data_need_keys": _data_need_keys(data_need_rows),
            "company_context": company_context_payload,
            "brief_en": (
                f"For {subject}, retrieve filing evidence for: {hook_text}. "
                f"User question: {question}"
            ),
            "boundary": (
                "This selector identifies evidence needs only; it does not answer company facts."
            ),
        }
    return {
        "use_existing_krw_ontology_mcp": False,
        "ticker": clean_ticker,
        "requires_identifier_clarification": False,
        "filing_evidence_requirements": [],
        "conditional_filing_evidence_requirements": list(evidence_requirements),
        "data_need_keys": _data_need_keys(data_need_rows),
        "company_context": company_context_payload,
        "next_step": "Select a company or ticker before calling KRW Ontology filing research.",
        "brief_en": (
            "No company or ticker is selected. Treat these as conditional filing checks "
            "for later company research, not as a request for company facts."
        ),
        "boundary": "Guru lens selection is separate from company fact retrieval.",
    }


def _answer_evidence_plan(
    *,
    question: str,
    intent_family: str | None,
    intent_families: Sequence[str],
    requires_company_evidence: bool,
    requires_identifier_clarification: bool,
    evidence_requirements: Sequence[str],
    data_need_rows: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    lens_specific = _lens_specific_requirements(data_need_rows)
    portfolio_context = []
    if "position_sizing" in intent_families:
        portfolio_context = [
            "current_position_weight",
            "cost_basis_or_unrealized_gain_loss",
            "decision_under_consideration",
            "risk_tolerance_or_max_drawdown",
        ]
    if requires_company_evidence:
        primary = list(dict.fromkeys(evidence_requirements))[:5]
    elif portfolio_context:
        primary = portfolio_context
    else:
        primary = []
    return {
        "mode": (
            "resolve_identifier_first"
            if requires_identifier_clarification
            else "filing_bridge_required"
            if requires_company_evidence
            else "guru_only"
        ),
        "primary_intent": intent_family,
        "secondary_intents": list(intent_families[1:]),
        "top_evidence_requirements": primary,
        "lens_specific_requirements": lens_specific[:8],
        "generic_filing_requirements": list(evidence_requirements[:8]),
        "portfolio_context_requirements": portfolio_context,
        "answer_guidance": (
            "Use top_evidence_requirements in normal answers; keep the longer filing list as "
            "backend research input."
        ),
    }


def _lens_specific_requirements(data_need_rows: Sequence[Mapping[str, Any]]) -> list[str]:
    requirements: list[str] = []
    for row in data_need_rows:
        requirements.extend(
            _normalized_company_hook(value) for value in row.get("company_data_hooks") or []
        )
        if row.get("data_need_key"):
            requirements.append(str(row["data_need_key"]))
    return list(dict.fromkeys(value for value in requirements if value))


def _selection_status(
    *,
    has_lenses: bool,
    requires_company_evidence: bool,
    clarifying_questions: Sequence[str],
    requires_identifier_clarification: bool,
) -> str:
    if not has_lenses:
        return "ontology_gap"
    if requires_identifier_clarification:
        return "needs_identifier_clarification"
    if clarifying_questions:
        return "needs_clarification"
    if requires_company_evidence:
        return "ready_for_company_bridge"
    return "ready_for_guru_only_answer"


def _data_need_keys(rows: Sequence[Mapping[str, Any]]) -> list[str]:
    return list(
        dict.fromkeys(
            str(row["data_need_key"]) for row in rows if row.get("data_need_key") not in (None, "")
        )
    )


def _dedupe_rows(rows: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    deduped: list[dict[str, Any]] = []
    seen: set[str] = set()
    for row in rows:
        reviewed_id = str(row.get("reviewed_id") or id(row))
        if reviewed_id in seen:
            continue
        seen.add(reviewed_id)
        deduped.append(dict(row))
    return deduped


def _selection_markdown(payload: Mapping[str, Any]) -> str:
    lines = [
        f"# Guru Lens Selection: {payload.get('question')}",
        "",
        f"- status: {payload.get('selection_status')}",
        f"- intent: {payload.get('intent_family')}",
        f"- requires company evidence: {payload.get('requires_company_evidence')}",
        "",
    ]
    for lens in payload.get("selected_lenses") or []:
        lines.append(f"- **{lens.get('label_ko')}** ({lens.get('author_name')})")
        lines.append(f"  - {lens.get('why_selected')}")
    return "\n".join(lines)
