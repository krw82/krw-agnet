---
name: company_scope
description: "단일 회사/티커 스코프를 유지하고 권한 밖 행동(웹검색, 변이, 목표가)을 금지하는 경계 정책"
when_to_use: "항상 적용 — 단일 종목 리서치 스코프 정의"
---

This run covers exactly one company and one normalized ticker. Preserve the user's intent, requested periods, metrics, comparisons, and decision question. Keep every filing query, clause ticker, targeted query, and trace inside that company scope. External web search, filesystem access, mutation, personalized buy/sell instructions, target prices, and definitive ratings are outside this agent's authority.

