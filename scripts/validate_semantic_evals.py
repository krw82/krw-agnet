#!/usr/bin/env python3
"""Validate deterministic KRW semantic fixtures without calling a model.

The suite is deliberately a contract test: it proves only that the checked
fixture artifacts obey the pinned rules. It is not a natural-language quality
score and must not be presented as one.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import re
import sys
import tempfile
from pathlib import Path
from typing import Any, Callable


HARD_FILE_LIMIT = 1024 * 1024
CASE_ID_RE = re.compile(r"^[a-z0-9][a-z0-9_.-]{2,95}$")
SHA256_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
HANGUL_RE = re.compile(r"[가-힣]")


class SuiteError(Exception):
    """An invalid suite, rather than an expected negative fixture."""


def _no_duplicate_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise SuiteError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def read_json(path: Path, maximum: int) -> tuple[Any, int]:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise SuiteError(f"cannot read {path}: {exc}") from exc
    if len(raw) > min(maximum, HARD_FILE_LIMIT):
        raise SuiteError(f"{path.name} exceeds byte limit")
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise SuiteError(f"{path.name} is not UTF-8") from exc
    try:
        value = json.loads(
            text,
            object_pairs_hook=_no_duplicate_object,
            parse_constant=lambda token: (_ for _ in ()).throw(
                SuiteError(f"non-finite JSON number: {token}")
            ),
        )
    except SuiteError:
        raise
    except json.JSONDecodeError as exc:
        raise SuiteError(f"invalid JSON in {path.name}: {exc}") from exc
    return value, len(raw)


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")


def digest(value: Any) -> str:
    return "sha256:" + hashlib.sha256(canonical_bytes(value)).hexdigest()


def require_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise SuiteError(f"{label} must be an object")
    return value


def require_list(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise SuiteError(f"{label} must be an array")
    return value


def validate_bounds(value: Any, limits: dict[str, Any], label: str = "root", depth: int = 0) -> None:
    if depth > int(limits["max_depth"]):
        raise SuiteError(f"{label} exceeds nesting limit")
    if value is None or isinstance(value, bool) or isinstance(value, int):
        return
    if isinstance(value, float):
        raise SuiteError(f"{label} contains a floating-point value; use exact integers or strings")
    if isinstance(value, str):
        if len(value.encode("utf-8")) > int(limits["max_string_bytes"]):
            raise SuiteError(f"{label} contains an oversized string")
        if any(ord(char) < 0x20 and char not in "\n\r\t" for char in value):
            raise SuiteError(f"{label} contains a forbidden control character")
        return
    if isinstance(value, list):
        if len(value) > int(limits["max_array_items"]):
            raise SuiteError(f"{label} exceeds array length limit")
        for index, item in enumerate(value):
            validate_bounds(item, limits, f"{label}[{index}]", depth + 1)
        return
    if isinstance(value, dict):
        if len(value) > int(limits["max_object_keys"]):
            raise SuiteError(f"{label} exceeds object key limit")
        for key, item in value.items():
            if not isinstance(key, str):
                raise SuiteError(f"{label} has a non-string key")
            validate_bounds(key, limits, f"{label}.<key>", depth + 1)
            validate_bounds(item, limits, f"{label}.{key}", depth + 1)
        return
    raise SuiteError(f"{label} contains unsupported value type {type(value).__name__}")


def merge_patch(base: Any, patch: Any) -> Any:
    """Apply RFC 7396 merge-patch semantics to an in-memory fixture."""
    if not isinstance(patch, dict):
        return copy.deepcopy(patch)
    result = copy.deepcopy(base) if isinstance(base, dict) else {}
    for key, value in patch.items():
        if value is None:
            result.pop(key, None)
        elif isinstance(value, dict):
            result[key] = merge_patch(result.get(key), value)
        else:
            result[key] = copy.deepcopy(value)
    return result


def unique_codes(codes: list[str]) -> list[str]:
    return sorted(set(codes))


def validate_router(candidate: dict[str, Any], policy: dict[str, Any]) -> list[str]:
    cfg = policy["router"]
    codes: list[str] = []
    decision = candidate.get("decision")
    if not isinstance(decision, dict) or sorted(decision) != sorted(cfg["output_keys"]):
        codes.append("router_output_shape")
    intent = candidate.get("intent")
    expected = cfg["intent_routes"].get(intent, cfg["default_run_kind"])
    if not isinstance(decision, dict) or decision.get("run_kind") != expected:
        codes.append("router_wrong_kind")
    elif decision.get("run_kind") not in cfg["allowed_run_kinds"]:
        codes.append("router_wrong_kind")
    return unique_codes(codes)


def validate_searchplan(candidate: dict[str, Any], policy: dict[str, Any]) -> list[str]:
    cfg = policy["searchplan"]
    codes: list[str] = []
    # `mcp_arguments` is the physical direct-root SearchPlan object. There is
    # no `{"search_plan": ...}` compatibility envelope to unwrap or accept.
    plan: Any = candidate.get("mcp_arguments")
    if not isinstance(plan, dict):
        return unique_codes(codes + ["searchplan_argument_shape"])

    plan_keys = set(plan)
    if not set(cfg["plan_required_keys"]).issubset(plan_keys) or not plan_keys.issubset(
        set(cfg["plan_allowed_keys"])
    ):
        codes.append("searchplan_argument_shape")
    clauses = plan.get("clauses")
    if not isinstance(clauses, list) or not 1 <= len(clauses) <= int(cfg["max_clauses"]):
        codes.append("searchplan_argument_shape")
        clauses = []
    clause_ids: list[str] = []
    for clause in clauses:
        if not isinstance(clause, dict):
            codes.append("searchplan_argument_shape")
            continue
        keys = set(clause)
        if not set(cfg["clause_required_keys"]).issubset(keys) or not keys.issubset(
            set(cfg["clause_allowed_keys"])
        ):
            codes.append("searchplan_argument_shape")
        clause_id = clause.get("clause_id")
        if not isinstance(clause_id, str) or not clause_id:
            codes.append("searchplan_argument_shape")
        else:
            clause_ids.append(clause_id)
        metrics = clause.get("metrics", [])
        concepts = clause.get("required_concepts", [])
        predicates = clause.get("required_predicates", [])
        if not all(isinstance(value, list) for value in (metrics, concepts, predicates)):
            codes.append("searchplan_argument_shape")
            continue
        if cfg["metric_predicates_mutually_exclusive"] and metrics and predicates:
            codes.append("searchplan_clause_atomicity")
        if (
            cfg["multi_concept_qualitative_requires_predicate"]
            and not metrics
            and len(concepts) > 1
            and not predicates
        ):
            codes.append("searchplan_clause_atomicity")
    if len(clause_ids) != len(set(clause_ids)):
        codes.append("searchplan_argument_shape")

    tickers = plan.get("tickers", [])
    if isinstance(tickers, list) and any(
        not isinstance(ticker, str) or ticker != ticker.upper() for ticker in tickers
    ):
        codes.append("searchplan_argument_shape")
    actions = candidate.get("action_sequence")
    if not isinstance(actions, list) or not actions or actions[0] != cfg["first_action"]:
        codes.append("searchplan_action_order")

    ledger = candidate.get("literal_ledger")
    if not isinstance(ledger, dict) or sorted(ledger) != sorted(cfg["literal_fields"]):
        codes.append("searchplan_literal_loss")
    else:
        actual: dict[str, list[Any]] = {
            "tickers": plan.get("tickers", []),
            "periods": plan.get("periods", []),
            "comparison_axes": plan.get("comparison_axes", []),
            "metrics": [
                metric
                for clause in clauses
                if isinstance(clause, dict)
                for metric in clause.get("metrics", [])
            ],
        }
        for field in cfg["literal_fields"]:
            required = ledger.get(field)
            if not isinstance(required, list):
                codes.append("searchplan_literal_loss")
                continue
            if field == "question_terms":
                question = plan.get("question")
                if not isinstance(question, str) or any(
                    not isinstance(literal, str) or literal not in question for literal in required
                ):
                    codes.append("searchplan_literal_loss")
                continue
            present = actual.get(field)
            if not isinstance(present, list) or any(literal not in present for literal in required):
                codes.append("searchplan_literal_loss")
    return unique_codes(codes)


def validate_evidence(candidate: dict[str, Any], policy: dict[str, Any]) -> list[str]:
    cfg = policy["evidence"]
    codes: list[str] = []
    claim = candidate.get("claim")
    premises = candidate.get("premises")
    if not isinstance(claim, dict) or not isinstance(premises, list):
        return ["evidence_directness_required"]
    strong = claim.get("strength") == "strong"
    if strong and candidate.get(cfg["strong_claim_global_flag"]) is not True:
        codes.append("evidence_strong_claim_disallowed")
    for premise in premises:
        if not isinstance(premise, dict) or premise.get("load_bearing") is not True:
            continue
        if strong and premise.get("coverage") != cfg["strong_claim_coverage"]:
            codes.append("evidence_clause_uncovered")
        expected_directness = (
            cfg["numeric_load_bearing_directness"]
            if claim.get("kind") == "numeric"
            else cfg["qualitative_load_bearing_directness"]
        )
        if strong and premise.get("directness") != expected_directness:
            codes.append("evidence_directness_required")
    return unique_codes(codes)


def validate_calculation(candidate: dict[str, Any], policy: dict[str, Any]) -> list[str]:
    cfg = policy["calculation"]
    codes: list[str] = []
    inputs = candidate.get("inputs")
    lineage = candidate.get("lineage_ids")
    output = candidate.get("output")
    claim = candidate.get("claim")
    if not isinstance(inputs, list) or not isinstance(lineage, list):
        return ["calculation_lineage_mismatch"]
    input_ids = [item.get("evidence_id") for item in inputs if isinstance(item, dict)]
    if len(input_ids) != len(inputs) or len(input_ids) != len(set(input_ids)) or set(input_ids) != set(lineage):
        codes.append("calculation_lineage_mismatch")
    if candidate.get("requires_aligned_inputs") is True and isinstance(output, dict):
        for field in cfg["aligned_fields"]:
            if any(not isinstance(item, dict) or item.get(field) != output.get(field) for item in inputs):
                codes.append("calculation_alignment_mismatch")
    if cfg["require_claim_output_alignment"]:
        if not isinstance(output, dict) or not isinstance(claim, dict):
            codes.append("calculation_alignment_mismatch")
        else:
            if claim.get("calculation_id") != candidate.get("calculation_id"):
                codes.append("calculation_alignment_mismatch")
            for field in cfg["aligned_fields"]:
                if claim.get(field) != output.get(field):
                    codes.append("calculation_alignment_mismatch")
    return unique_codes(codes)


def validate_period(candidate: dict[str, Any], policy: dict[str, Any]) -> list[str]:
    cfg = policy["period"]
    codes: list[str] = []
    filings = candidate.get("filings")
    selected = candidate.get("selected")
    if not isinstance(filings, list) or not isinstance(selected, dict):
        return ["period_role_mismatch"]
    confirmed = [
        filing
        for filing in filings
        if isinstance(filing, dict)
        and filing.get("confirmed") is True
        and filing.get("form") in cfg["current_forms"]
        and isinstance(filing.get("recency_rank"), int)
    ]
    annual = [filing for filing in confirmed if filing.get("form") == cfg["annual_form"]]
    if not confirmed or not annual:
        codes.append("period_role_mismatch")
    else:
        expected_current = max(confirmed, key=lambda item: item["recency_rank"])["period"]
        expected_annual = max(annual, key=lambda item: item["recency_rank"])["period"]
        if selected.get("current_driver") != expected_current or selected.get("annual_baseline") != expected_annual:
            codes.append("period_role_mismatch")
    confirmed_labels = {
        filing.get("period")
        for filing in confirmed
        if isinstance(filing.get("period"), str)
    }
    user_future = candidate.get("user_supplied_future_labels", [])
    visible = candidate.get("visible_period_labels", [])
    if not isinstance(user_future, list) or not isinstance(visible, list):
        codes.append("period_unconfirmed_label")
    else:
        allowed = confirmed_labels | {label for label in user_future if isinstance(label, str)}
        if any(not isinstance(label, str) or label not in allowed for label in visible):
            codes.append("period_unconfirmed_label")
    return unique_codes(codes)


def validate_answer(candidate: dict[str, Any], policy: dict[str, Any]) -> list[str]:
    cfg = policy["answer"]
    codes: list[str] = []
    followups = candidate.get("followups")
    markers = candidate.get("followup_markers")
    if (
        not isinstance(followups, list)
        or len(followups) != int(cfg["follow_up_count"])
        or not all(isinstance(item, str) and item.strip() and HANGUL_RE.search(item) for item in followups)
        or markers != cfg["follow_up_markers"]
    ):
        codes.append("answer_followup_contract")
    visible_parts: list[str] = []
    for field in ("visible_text",):
        if isinstance(candidate.get(field), str):
            visible_parts.append(candidate[field])
    if isinstance(followups, list):
        visible_parts.extend(item for item in followups if isinstance(item, str))
    visible = "\n".join(visible_parts).casefold()
    if any(term.casefold() in visible for term in cfg["forbidden_internal_terms"]):
        codes.append("answer_internal_term")
    headings = candidate.get("headings")
    if not isinstance(headings, list):
        codes.append("answer_decision_heading_contract")
    elif candidate.get("decision_triggered") is True:
        if headings != cfg["decision_headings"] or candidate.get("opening_before_heading") is not True:
            codes.append("answer_decision_heading_contract")
    elif any(heading in cfg["decision_headings"] for heading in headings):
        codes.append("answer_decision_heading_contract")
    return unique_codes(codes)


def validate_news(candidate: dict[str, Any], policy: dict[str, Any]) -> list[str]:
    cfg = policy["news"]
    codes: list[str] = []
    path = candidate.get("direct_company_path")
    has_path = isinstance(path, dict) and all(
        path.get(field) is True for field in ("direct", "company_specific", "complete")
    )
    if candidate.get("conclusion_shaped") is True and not has_path:
        codes.append("news_direct_path_required")
    if candidate.get("conclusion_shaped") is not True and not has_path:
        if candidate.get("safe_sections") != cfg["safe_sections_without_direct_path"]:
            codes.append("news_safe_boundary")
        if cfg["forbid_financial_scenario_without_direct_path"] and candidate.get("financial_scenario") is True:
            codes.append("news_safe_boundary")
    return unique_codes(codes)


def validate_idea(candidate: dict[str, Any], policy: dict[str, Any]) -> list[str]:
    cfg = policy["idea"]
    priority = candidate.get("priority")
    valid = priority in cfg["priorities"]
    if cfg["forbid_rating_language"] and candidate.get("rating_language") is not False:
        valid = False
    if priority == "A":
        valid = valid and all(candidate.get(field) is True for field in cfg["a_required_true"])
        valid = valid and candidate.get("material_gap_count") == 0 and candidate.get("advancing_supported") is True
    elif priority == "B":
        valid = valid and candidate.get("credible_pathway") is True
        valid = valid and candidate.get("material_gap_count") == cfg["b_material_gap_count"]
        valid = valid and candidate.get("advancing_supported") is True
    elif priority == "C":
        has_weakness = any(
            candidate.get(field) is not True
            for field in ("direct_exposure", "recent_filing_evidence", "financial_path")
        )
        valid = valid and has_weakness and candidate.get("advancing_supported") is True
    elif priority == "Reject":
        valid = valid and candidate.get("advancing_supported") is False
    return [] if valid else ["idea_priority_floor"]


def validate_scenario(candidate: dict[str, Any], policy: dict[str, Any]) -> list[str]:
    cfg = policy["scenario"]
    inputs = candidate.get("inputs")
    valid = isinstance(inputs, list) and bool(inputs)
    if not isinstance(inputs, list):
        return ["scenario_provenance_boundary"]
    for item in inputs:
        if not isinstance(item, dict):
            valid = False
            continue
        category = item.get("category")
        if category not in cfg["categories"] or item.get("source_type") != cfg["source_by_category"].get(category):
            valid = False
        if category in cfg["evidence_required_categories"]:
            evidence_ids = item.get("evidence_ids")
            if not isinstance(evidence_ids, list) or not evidence_ids:
                valid = False
        if category in ("filing_assumption", "user_assumption", "analyst_inference") and item.get(
            "presented_as_company_fact"
        ) is not False:
            valid = False
    return [] if valid else ["scenario_provenance_boundary"]


def validate_guru(candidate: dict[str, Any], policy: dict[str, Any]) -> list[str]:
    cfg = policy["guru"]
    codes: list[str] = []
    advisor = candidate.get("selected_advisor")
    if advisor not in cfg["author_by_advisor"] or candidate.get("author_key") != cfg["author_by_advisor"].get(advisor):
        codes.append("guru_author_mismatch")

    questions = candidate.get("key_questions")
    if not isinstance(questions, list) or len(questions) != int(cfg["max_key_questions"]):
        codes.append("guru_question_contract")
        questions = []
    for question in questions:
        if not isinstance(question, dict):
            codes.append("guru_question_contract")
            continue
        if question.get("central_tension_count") != 1 or not isinstance(question.get("evidence_clause_count"), int) or not 1 <= question["evidence_clause_count"] <= 3:
            codes.append("guru_question_contract")
        original = question.get("sealed_original")
        used = question.get("sealed_used")
        if not isinstance(original, dict) or not isinstance(used, dict):
            codes.append("guru_seal_changed")
            continue
        sealed_fields = cfg["sealed_fields"]
        if sorted(original) != sorted(sealed_fields) or sorted(used) != sorted(sealed_fields):
            codes.append("guru_seal_changed")
        elif any(original.get(field) != used.get(field) for field in sealed_fields):
            codes.append("guru_seal_changed")
        if not SHA256_RE.fullmatch(str(original.get("brief_hash", ""))):
            codes.append("guru_seal_changed")

    if (
        candidate.get("workflow") != cfg["workflow_order"]
        or candidate.get("verify_evidence_call_count") != cfg["verify_evidence_calls"]
        or candidate.get("verifier_payload_unchanged") is not True
        or not isinstance(candidate.get("correction_call_count"), int)
        or not 0 <= candidate["correction_call_count"] <= int(cfg["max_correction_calls"])
    ):
        codes.append("guru_workflow_order")

    review = candidate.get("review_input")
    if not isinstance(review, dict):
        codes.append("guru_review_shape")
        review = {}
    runtime_fields = set(cfg["runtime_owned_review_fields"])
    if set(review) & runtime_fields:
        codes.append("guru_runtime_field")
    model_keys = set(review) - runtime_fields
    if model_keys != set(cfg["review_input_keys"]):
        codes.append("guru_review_shape")
    analysis = review.get("agent_analysis")
    if not isinstance(review.get("question"), str) or not review.get("question", "").strip() or not isinstance(analysis, dict):
        codes.append("guru_review_shape")
        assessments: list[Any] = []
    else:
        assessments = analysis.get("assessments", [])
        if not isinstance(assessments, list) or not assessments or not isinstance(
            analysis.get("overall_judgment"), str
        ) or not analysis.get("overall_judgment", "").strip():
            codes.append("guru_review_shape")
            assessments = [] if not isinstance(assessments, list) else assessments
    if candidate.get("answerability") == "interpretation_only":
        for assessment in assessments:
            if not isinstance(assessment, dict) or assessment.get("verdict") not in cfg["interpretation_only_verdicts"]:
                codes.append("guru_verdict_boundary")

    voice = candidate.get("voice")
    if not isinstance(voice, dict) or any(
        voice.get(field) is not False
        for field in ("claims_real_person", "first_person_as_author", "fabricated_quote", "fabricated_holding")
    ):
        codes.append("guru_impersonation")
    return unique_codes(codes)


VALIDATORS: dict[str, Callable[[dict[str, Any], dict[str, Any]], list[str]]] = {
    "router": validate_router,
    "searchplan": validate_searchplan,
    "evidence": validate_evidence,
    "calculation": validate_calculation,
    "period": validate_period,
    "answer": validate_answer,
    "news": validate_news,
    "idea": validate_idea,
    "scenario": validate_scenario,
    "guru": validate_guru,
}


def verify_policy(policy: dict[str, Any]) -> None:
    if policy.get("schema_version") != "krw-semantic-policy/v1":
        raise SuiteError("unsupported policy schema_version")
    limits = require_object(policy.get("limits"), "policy.limits")
    required_limits = {
        "max_policy_bytes",
        "max_cases_bytes",
        "max_manifest_bytes",
        "max_cases",
        "max_templates",
        "max_case_bytes",
        "max_depth",
        "max_object_keys",
        "max_array_items",
        "max_string_bytes",
    }
    if set(limits) != required_limits or any(
        not isinstance(value, int) or isinstance(value, bool) or value <= 0 for value in limits.values()
    ):
        raise SuiteError("policy limits must be exact positive integers")
    gates = require_object(policy.get("gates"), "policy.gates")
    rule_ids = require_list(gates.get("required_rule_ids"), "policy.gates.required_rule_ids")
    if not rule_ids or len(rule_ids) != len(set(rule_ids)) or not all(
        isinstance(rule_id, str) and CASE_ID_RE.fullmatch(rule_id) for rule_id in rule_ids
    ):
        raise SuiteError("required_rule_ids must be unique stable identifiers")
    router = require_object(policy.get("router"), "policy.router")
    allowed = require_list(router.get("allowed_run_kinds"), "policy.router.allowed_run_kinds")
    if router.get("default_run_kind") not in allowed or any(
        value not in allowed for value in require_object(router.get("intent_routes"), "intent_routes").values()
    ):
        raise SuiteError("router policy references an unknown run_kind")
    guru = require_object(policy.get("guru"), "policy.guru")
    expected_authors = {"ackman", "buffett", "flatt", "marks", "terry_smith"}
    if set(require_object(guru.get("author_by_advisor"), "author_by_advisor")) != expected_authors:
        raise SuiteError("guru author map must pin all five advisors")


def verify_manifest(manifest: dict[str, Any], policy: dict[str, Any], cases: dict[str, Any]) -> None:
    expected_keys = {"schema_version", "suite_id", "suite_version", "hash_algorithm", "files", "case_count"}
    if set(manifest) != expected_keys:
        raise SuiteError("manifest keys do not match the v1 contract")
    if manifest.get("schema_version") != "krw-semantic-manifest/v1":
        raise SuiteError("unsupported manifest schema_version")
    if manifest.get("hash_algorithm") != "sha256-canonical-json-v1":
        raise SuiteError("unsupported manifest hash algorithm")
    for field in ("suite_id", "suite_version"):
        if manifest.get(field) != policy.get(field) or manifest.get(field) != cases.get(field):
            raise SuiteError(f"suite {field} mismatch")
    files = require_object(manifest.get("files"), "manifest.files")
    if set(files) != {"policy.json", "cases.json"}:
        raise SuiteError("manifest must pin exactly policy.json and cases.json")
    expected_hashes = {"policy.json": digest(policy), "cases.json": digest(cases)}
    if files != expected_hashes:
        raise SuiteError("manifest content hash mismatch")
    if manifest.get("case_count") != len(require_list(cases.get("cases"), "cases.cases")):
        raise SuiteError("manifest case_count mismatch")


def expand_and_verify_cases(
    cases_doc: dict[str, Any], policy: dict[str, Any]
) -> tuple[list[dict[str, Any]], dict[str, dict[str, int]]]:
    if cases_doc.get("schema_version") != "krw-semantic-cases/v1":
        raise SuiteError("unsupported cases schema_version")
    if cases_doc.get("suite_id") != policy.get("suite_id") or cases_doc.get("suite_version") != policy.get(
        "suite_version"
    ):
        raise SuiteError("cases suite identity mismatch")
    limits = policy["limits"]
    templates = require_object(cases_doc.get("templates"), "cases.templates")
    raw_cases = require_list(cases_doc.get("cases"), "cases.cases")
    if len(templates) > int(limits["max_templates"]):
        raise SuiteError("too many templates")
    if not raw_cases or len(raw_cases) > int(limits["max_cases"]):
        raise SuiteError("case count is outside bounds")
    for name, template in templates.items():
        if not CASE_ID_RE.fullmatch(name) or not isinstance(template, dict):
            raise SuiteError(f"invalid template {name}")

    allowed_case_keys = {
        "id",
        "validator",
        "template",
        "candidate",
        "patch",
        "covers",
        "expected_pass",
        "expected_codes",
    }
    required_rules = set(policy["gates"]["required_rule_ids"])
    coverage = {rule: {"positive": 0, "negative": 0} for rule in sorted(required_rules)}
    seen_ids: set[str] = set()
    expanded: list[dict[str, Any]] = []
    for index, raw_case in enumerate(raw_cases):
        case = require_object(raw_case, f"cases[{index}]")
        if not set(case).issubset(allowed_case_keys):
            raise SuiteError(f"case {index} contains unknown fields")
        case_id = case.get("id")
        if not isinstance(case_id, str) or not CASE_ID_RE.fullmatch(case_id) or case_id in seen_ids:
            raise SuiteError(f"invalid or duplicate case id at index {index}")
        seen_ids.add(case_id)
        validator = case.get("validator")
        if validator not in VALIDATORS:
            raise SuiteError(f"{case_id} has unknown validator")
        has_template = "template" in case
        has_candidate = "candidate" in case
        if has_template == has_candidate:
            raise SuiteError(f"{case_id} must use exactly one of template or candidate")
        if has_template:
            template_name = case.get("template")
            if template_name not in templates:
                raise SuiteError(f"{case_id} references unknown template")
            candidate = merge_patch(templates[template_name], case.get("patch", {}))
        else:
            if "patch" in case:
                raise SuiteError(f"{case_id} cannot patch an inline candidate")
            candidate = copy.deepcopy(case.get("candidate"))
        if not isinstance(candidate, dict):
            raise SuiteError(f"{case_id} candidate must be an object")
        validate_bounds(candidate, limits, f"candidate.{case_id}")
        if len(canonical_bytes(candidate)) > int(limits["max_case_bytes"]):
            raise SuiteError(f"{case_id} resolved candidate exceeds max_case_bytes")
        covers = case.get("covers")
        if not isinstance(covers, list) or not covers or len(covers) != len(set(covers)) or not set(covers).issubset(
            required_rules
        ):
            raise SuiteError(f"{case_id} has invalid rule coverage")
        expected_pass = case.get("expected_pass")
        expected_codes = case.get("expected_codes")
        if not isinstance(expected_pass, bool) or not isinstance(expected_codes, list) or not all(
            isinstance(code, str) and CASE_ID_RE.fullmatch(code) for code in expected_codes
        ) or expected_codes != sorted(set(expected_codes)):
            raise SuiteError(f"{case_id} has invalid expectations")
        if expected_pass != (len(expected_codes) == 0):
            raise SuiteError(f"{case_id} expected_pass conflicts with expected_codes")
        polarity = "positive" if expected_pass else "negative"
        for rule in covers:
            coverage[rule][polarity] += 1
        expanded.append(
            {
                "id": case_id,
                "validator": validator,
                "candidate": candidate,
                "covers": sorted(covers),
                "expected_pass": expected_pass,
                "expected_codes": expected_codes,
            }
        )
    if policy["gates"]["require_positive_and_negative_per_rule"]:
        missing = [rule for rule, counts in coverage.items() if not counts["positive"] or not counts["negative"]]
        if missing:
            raise SuiteError("rules missing positive/negative fixture coverage: " + ", ".join(missing))
    return expanded, coverage


def evaluate(
    cases: list[dict[str, Any]], policy: dict[str, Any], coverage: dict[str, dict[str, int]], manifest: dict[str, Any]
) -> tuple[dict[str, Any], int]:
    results: list[dict[str, Any]] = []
    failure_count = 0
    for case in cases:
        observed = VALIDATORS[case["validator"]](case["candidate"], policy)
        expected = case["expected_codes"]
        matched = observed == expected if policy["gates"]["require_exact_expected_codes"] else set(expected).issubset(observed)
        if not matched:
            failure_count += 1
        results.append(
            {
                "id": case["id"],
                "validator": case["validator"],
                "covers": case["covers"],
                "expected_codes": expected,
                "observed_codes": observed,
                "matched": matched,
            }
        )
    report_core: dict[str, Any] = {
        "schema_version": "krw-semantic-report/v1",
        "suite_id": policy["suite_id"],
        "suite_version": policy["suite_version"],
        "scope": "deterministic_fixture_contract_only",
        "manifest_hash": digest(manifest),
        "status": "pass" if failure_count == 0 else "fail",
        "summary": {
            "case_count": len(cases),
            "matched": len(cases) - failure_count,
            "mismatched": failure_count,
            "rule_count": len(coverage),
        },
        "coverage": coverage,
        "results": results,
    }
    report_core["report_hash"] = digest(report_core)
    return report_core, 0 if failure_count == 0 else 1


def write_report(path: Path, report: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(report, ensure_ascii=False, sort_keys=True, indent=2) + "\n"
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=str(path.parent))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def failure_report(message: str) -> dict[str, Any]:
    report: dict[str, Any] = {
        "schema_version": "krw-semantic-report/v1",
        "scope": "deterministic_fixture_contract_only",
        "status": "invalid_suite",
        "error": message,
    }
    report["report_hash"] = digest(report)
    return report


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--suite",
        type=Path,
        default=Path("evals/krw-semantic/v2"),
        help="directory containing policy.json, cases.json, and manifest.json",
    )
    parser.add_argument("--report", type=Path, help="optional atomic JSON report output path")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        policy_value, policy_size = read_json(args.suite / "policy.json", HARD_FILE_LIMIT)
        policy = require_object(policy_value, "policy")
        verify_policy(policy)
        limits = policy["limits"]
        if policy_size > int(limits["max_policy_bytes"]):
            raise SuiteError("policy.json exceeds its declared limit")
        cases_value, cases_size = read_json(args.suite / "cases.json", int(limits["max_cases_bytes"]))
        manifest_value, manifest_size = read_json(
            args.suite / "manifest.json", int(limits["max_manifest_bytes"])
        )
        cases_doc = require_object(cases_value, "cases")
        manifest = require_object(manifest_value, "manifest")
        if cases_size > int(limits["max_cases_bytes"]) or manifest_size > int(limits["max_manifest_bytes"]):
            raise SuiteError("suite file exceeds declared limit")
        validate_bounds(policy, limits, "policy")
        validate_bounds(cases_doc, limits, "cases")
        validate_bounds(manifest, limits, "manifest")
        verify_manifest(manifest, policy, cases_doc)
        expanded, coverage = expand_and_verify_cases(cases_doc, policy)
        report, status = evaluate(expanded, policy, coverage, manifest)
    except (SuiteError, OSError, KeyError, TypeError, ValueError) as exc:
        report = failure_report(str(exc))
        status = 2
    if args.report:
        try:
            write_report(args.report, report)
        except OSError as exc:
            report = failure_report(f"cannot write report: {exc}")
            status = 2
    print(json.dumps(report, ensure_ascii=False, sort_keys=True, indent=2))
    return status


if __name__ == "__main__":
    sys.exit(main())
