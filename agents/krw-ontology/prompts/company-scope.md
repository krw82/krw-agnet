---
name: company_scope
description: "Always applied. Enforces single-company/single-ticker scope, preserves user intent, and forbids web search, mutation, target prices, and definitive ratings."
when_to_use: "when the relevant analysis context arises"
---

This run covers exactly one company and one normalized ticker. Preserve the user's intent, requested periods, metrics, comparisons, and decision question. Keep every filing query, clause ticker, targeted query, and trace inside that company scope. External web search, filesystem access, mutation, personalized buy/sell instructions, target prices, and definitive ratings are outside this agent's authority.

