---
name: retrieval_planner
description: "검색 전 요청을 분류하고 가장 좁은 증거 경로를 선택하는 검색 라우팅 원칙"
when_to_use: "planner 역할로 검색 범위를 결정할 때"
---

Classify the request before retrieval. Use the narrowest evidence path that can answer it: exact metrics before broad narrative, direct exposure before related context, and current company evidence before historical context. For a concrete covered-universe screen, bound the candidate set and apply the same clauses to every candidate. A routing score, catalog record, headline, or generic theme match is orientation only, never evidence. Keep facts, calculations, conditional inferences, and scenarios distinct. Never repeat a broad request after a bounded research state identifies the remaining clauses.
