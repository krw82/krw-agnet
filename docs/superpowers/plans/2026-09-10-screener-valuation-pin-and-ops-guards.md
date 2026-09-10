# Screener valuation lens pin + e2e ops guards (2026-09-10)

Outcome of the equivalence-mismatch agent debate (see memory:
`equivalence-mismatch-debate-2026-09-10`). Two approved work items, both in
this change set.

## P1 — pin the finviz screener metric lens to performance/overview

**Defect.** `openbb.screener` with `metric=valuation` dies deterministically:
finviz's valuation payload ships `eps_growth_past_1y='-'` rows that the OpenBB
provider fails to parse into numbers, surfacing as a retryable provider 500
(`provider_tool_error_text`, retryable=true). The engine defers, re-claims,
and repeats until the 16-defer cap (`deferred_attempts_exhausted`) kills the
run. `metric=performance` and `metric=overview` return 200.

**Fix (contract refusal up front, not runtime retries).** Remove `valuation`
from the metric enum across the pin chain:

- `contracts/kernel/v1/schemas/openbb-screener-request-v1.json`
- `contracts/kernel/v1/schemas/openbb-screener-input-v1.json`
- `crates/krw-contracts/src/lib.rs` — `validate_openbb_screener_request`
  and `validate_openbb_screener_input` matches, plus the two
  `*_SCHEMA_SHA256` constants (actuals from the registry test).
- `crates/run-engine/src/capability_dispatch.rs` — screener-input derive
  filter.
- `agents/krw-ontology/agent.yaml` — both `content_hash` pins.

The **sector-performance lane keeps `valuation`**: its finviz groups endpoint
does not exhibit the parse failure, and the debate scoped P1 to the screener
contract only. A valuation ask now either routes to
`openbb.sector_performance` (group lens) or per-ticker `openbb.metrics`,
arriving as a contract `Shape` rejection on `openbb.screener` — a repairable
policy error, not a defer loop.

**Scope gates (all green):** `cargo test -p krw-agent-contracts` (37),
`-p krw-agent-run-engine` (192), `-p krw-agent-image` (33).

## Monitoring baseline (recorded before this reboot wiped the ledger)

Measured on the 2026-09-10 debate data (pre-wipe throwaway ledger + logs):

- recovery equivalence-mismatch WARNs: **0** (the four parallel-session
  replay fixes — ec9551e / 416ed77 / 199775f / b6889ef — held; any non-zero
  reading after this change is a regression, not noise).
- `provider_tool_error_text` occurrences: **487** (dominated by the finviz
  screener valuation 500 loop).
- `deferred_attempts` histogram: **{16: 20}** — 20 runs dead at the 16-defer
  cap.

Post-change expectations: the {16:N} bucket disappears, screener-lens
`provider_tool_error_text` drops; residual provider errors should come from
unrelated lanes only.

## Ops guard — restart-stack.sh preserves forensic material

`restart-stack.sh` wipes `.local/e2e-open/postgres` and the boot's logs on
every reboot; a run that failed minutes before a restart became
undiagnosable. The script now archives, before teardown:

- `.local/e2e-open/logs/agentd.log` → `.local/e2e-open/archives/<ts>/`
- `pg_dump` of the throwaway ledger (port 55433, trust auth) →
  `archives/<ts>/ledger.sql` (failure is non-fatal: warn and continue — the
  guard must never block a reboot).

Archives live under gitignored `.local/`; prune old timestamped dirs manually
when they accumulate.

## Live gates (after commit + stack reboot)

1. Valuation-routing question through the ChatBox adapter: run must complete
   without `deferred_attempts_exhausted`; the screener contract must refuse
   `metric=valuation` with a Shape rejection and the model must reroute
   (sector-performance group lens / per-ticker metrics).
2. MSFT control question: unchanged behavior — silent surface (keepalive
   only), complete answer, no evidence footer.
