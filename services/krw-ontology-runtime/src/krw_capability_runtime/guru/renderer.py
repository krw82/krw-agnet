"""Render-plan helpers for guru advisor answers.

The renderer returns a plan rather than final prose. It gives the answering
skill stable structure and author-specific voice posture while keeping the
actual answer agent free to write naturally from the ResearchPack.
"""

from __future__ import annotations

from typing import Any, Mapping

from krw_capability_runtime.guru.models import GuruAnswerRenderPlan, GuruCompanyResearchPack


AUTHOR_OPENINGS = {
    "buffett": "자, 내가 먼저 묻고 싶은 건 하나입니다.",
    "marks": "질문을 조금 바꿔야 합니다.",
    "ackman": "좋습니다. 이건 감정이 아니라 논리로 쪼개야 합니다.",
    "flatt": "먼저 자산이 시간을 견딜 수 있는 구조인지 봐야 합니다.",
    "terry_smith": "나는 먼저 아주 단순한 질문부터 하겠습니다.",
}

AUTHOR_NEXT_QUESTIONS = {
    "buffett": "이 회사를 통째로 산다고 해도 같은 결론인가?",
    "marks": "내가 틀렸을 때 손실 경로는 어디서 시작되는가?",
    "ackman": "가치를 풀 수 있는 통제 가능한 레버가 실제로 있는가?",
    "flatt": "장기 계약, 자본 구조, 재투자 기회가 시간을 내 편으로 만드는가?",
    "terry_smith": "이 사업은 좋은 자본수익률을 현금으로 바꿔 오래 유지할 수 있는가?",
}

FORBIDDEN_OUTPUT_PATTERNS = [
    "참고:",
    "주의:",
    "위 평가는",
    "AI 렌즈 해석",
    "실제 워런 버핏 본인의 조언",
    "실제 하워드 막스 본인의 조언",
    "매수·매도·목표가 등의 구체적 투자 지시",
    "목표가나 매수/매도 결론",
    "데이터 한계",
    "데이터 한계 (정직하게)",
    "정확한 결론을 내리려면 아래를 별도로 보강해야 합니다",
    "임의의 수치/목표가",
    "렌즈로 보면",
    "렌즈로 본다면",
    "관점에서 보면",
    "관점에서 분석하면",
    "버핏이라면",
    "막스라면",
    "애크먼이라면",
    "플랫이라면",
    "테리 스미스라면",
    "버핏식 결론",
    "막스식 결론",
    "애크먼식 결론",
    "플랫식 결론",
    "테리 스미스식 결론",
    "ResearchPack",
    "MCP",
    "reviewed_id",
    "schema",
    "curation",
    "batch",
]

TOPIC_LABELS = {
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


def build_guru_answer_render_plan(
    company_pack: GuruCompanyResearchPack | Mapping[str, Any],
) -> GuruAnswerRenderPlan:
    """Build a bounded answer rendering plan from a GuruCompanyResearchPack."""

    pack = _pack_dict(company_pack)
    author_key = str(pack.get("author_key") or "buffett")
    guru_pack = _mapping(pack.get("guru_pack"))
    selected_lenses = _list_of_mappings(guru_pack.get("selected_lenses"))
    top_lens = selected_lenses[0] if selected_lenses else {}
    top_label = str(top_lens.get("label_ko") or "가장 중요한 판단 기준")
    top_summary = str(top_lens.get("summary_ko") or "")
    company_identity = _mapping(pack.get("company_identity"))
    subject = str(company_identity.get("subject") or "이 회사")
    alignments = _list_of_mappings(pack.get("evidence_alignment"))
    supported_points = _supported_points(alignments)
    missing = [str(item) for item in pack.get("missing_evidence") or [] if item]
    concerns = _concerns_from_missing(missing)
    first_question = _first_question(
        author_key=author_key,
        subject=subject,
        top_label=top_label,
        missing=missing,
    )
    reframe = _reframe(
        author_key=author_key,
        subject=subject,
        top_label=top_label,
        top_summary=top_summary,
    )
    next_question = AUTHOR_NEXT_QUESTIONS.get(
        author_key,
        "이 판단을 바꾸는 핵심 근거는 무엇인가?",
    )
    if missing:
        next_question = (
            f"{next_question} 내가 더 확인할 건 "
            f"{_humanize_topics(missing[:3])}입니다."
        )
    return GuruAnswerRenderPlan(
        author_key=author_key,
        opening_style=AUTHOR_OPENINGS.get(author_key, AUTHOR_OPENINGS["buffett"]),
        first_question=first_question,
        reframe=reframe,
        supported_points=supported_points,
        concerns=concerns,
        missing_evidence=missing,
        judgment_conditions=dict(_mapping(pack.get("judgment_conditions"))),
        next_question=next_question,
        forbidden_output_patterns=FORBIDDEN_OUTPUT_PATTERNS,
        source_fields=[
            "guru_pack.selected_lenses",
            "guru_pack.consultation_moves",
            "company_evidence_pack",
            "evidence_alignment",
            "missing_evidence",
        ],
    )


def _first_question(
    *,
    author_key: str,
    subject: str,
    top_label: str,
    missing: list[str],
) -> str:
    quoted_label = f"'{top_label}'"
    if author_key == "buffett":
        return f"내가 먼저 볼 건 {subject}를 사업 일부로 가져도 {quoted_label}이 실제 숫자로 버티는지입니다."
    if author_key == "marks":
        return f"내가 먼저 조심할 건 {subject}의 좋은 이야기가 아니라, {quoted_label}이 가격과 하방에 어떻게 들어갔는지입니다."
    if author_key == "ackman":
        return f"내가 먼저 쓸 가설은 이겁니다: {subject}에서 {quoted_label}을 숫자와 행동으로 증명할 수 있는가."
    if author_key == "flatt":
        return f"내가 먼저 볼 건 {subject}가 장기간 자본을 묶어둘 만한 구조를 갖췄는지입니다."
    if author_key == "terry_smith":
        return f"내가 먼저 걸러낼 건 {subject}가 복잡한 이야기 없이 {quoted_label}을 오래 반복할 수 있느냐입니다."
    if missing:
        return f"{subject} 판단 전에 {', '.join(missing[:3])}부터 확인해야 합니다."
    return f"내가 먼저 확인할 건 {subject}의 {quoted_label}입니다."


def _reframe(*, author_key: str, subject: str, top_label: str, top_summary: str) -> str:
    summary = f" {top_summary}" if top_summary else ""
    if author_key == "buffett":
        return f"가격표보다 먼저 {subject}라는 장사가 {top_label}을 보여주는지 보겠습니다.{summary}"
    if author_key == "marks":
        return f"핵심은 {subject}가 좋은가만이 아니라, {top_label}에 대한 기대가 이미 얼마나 들어갔느냐입니다.{summary}"
    if author_key == "ackman":
        return f"좋습니다. {subject}의 논리는 {top_label}이 숫자와 행동으로 증명되는지에 달려 있습니다.{summary}"
    if author_key == "flatt":
        return f"{subject}는 단기 뉴스보다 {top_label}이 오래 지속되는 구조인지가 중요합니다.{summary}"
    if author_key == "terry_smith":
        return f"{subject}는 복잡한 촉매보다 {top_label}이 단순하고 반복 가능한지부터 봅니다.{summary}"
    return f"{subject} 판단은 {top_label}에서 시작합니다.{summary}"


def _supported_points(alignments: list[Mapping[str, Any]]) -> list[str]:
    points: list[str] = []
    for item in alignments:
        status = item.get("status")
        label = item.get("lens_label_ko") or "선택된 판단 기준"
        if status == "supported":
            points.append(f"{label}: 필요한 회사 공시 근거가 연결됨")
        elif status == "partial":
            points.append(f"{label}: 일부 회사 공시 근거만 연결됨")
    return points


def _concerns_from_missing(missing: list[str]) -> list[str]:
    if not missing:
        return []
    return [f"아직 단정하면 안 되는 부분은 {_humanize_topics(missing[:3])}입니다."]


def _humanize_topics(topics: list[str]) -> str:
    labels = [TOPIC_LABELS.get(item, item.replace("_", " ")) for item in topics if item]
    if len(labels) <= 1:
        return labels[0] if labels else "핵심 회사 근거"
    if len(labels) == 2:
        return f"{labels[0]}와 {labels[1]}"
    return f"{', '.join(labels[:-1])}, 그리고 {labels[-1]}"


def _pack_dict(value: GuruCompanyResearchPack | Mapping[str, Any]) -> dict[str, Any]:
    if isinstance(value, GuruCompanyResearchPack):
        return value.model_dump(mode="json", exclude_none=True)
    return dict(value)


def _mapping(value: Any) -> Mapping[str, Any]:
    return value if isinstance(value, Mapping) else {}


def _list_of_mappings(value: Any) -> list[Mapping[str, Any]]:
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, Mapping)]
