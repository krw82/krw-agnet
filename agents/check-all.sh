#!/usr/bin/env bash
set -euo pipefail

agent_root="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$agent_root/.." && pwd)"
catalog="$agent_root/fixtures/entrypoints.tsv"
semantic_markers="$agent_root/fixtures/semantic-markers.tsv"

feed_binding_keys=(
  list_feed_items
  get_feed_items
  get_feed_context
  search_catalog_filings
  krw_ontology_query_context
  krw_ontology_query
  krw_ontology_trace
)

source_filing_binding_keys=(
  get_filing
  get_filing_brief
  list_filing_sections
  read_filing_section
  list_filing_documents
  read_filing_document
  get_form4_insider_transactions
)

guru_binding_keys=(
  krw_guru_query_context
  krw_guru_company_brief
  krw_ontology_query_context
  krw_ontology_query
  krw_ontology_trace
  krw_ontology_chain
  krw_guru_review_company_evidence
)

fail() {
  printf 'agent source check failed: %s\n' "$1" >&2
  exit 1
}

while IFS=$'\t' read -r package entrypoint run_kind workflow; do
  case "$package" in
    ''|'#'*) continue ;;
  esac
  spec="$agent_root/$package/agent.yaml"
  test -f "$spec" || fail "missing $spec"
  rg -q "^  ${entrypoint}:$" "$spec" || fail "$package missing entrypoint $entrypoint"
  rg -q "^    run_kind: ${run_kind}$" "$spec" || fail "$package missing run_kind $run_kind"
  rg -q "^  - id: ${workflow}$" "$spec" || fail "$package missing workflow $workflow"
done < "$catalog"

while IFS=$'\t' read -r relative_path invariant literal_marker; do
  case "$relative_path" in
    ''|'#'*) continue ;;
  esac
  source_file="$agent_root/$relative_path"
  test -f "$source_file" || fail "missing semantic source $relative_path"
  rg -F -q -- "$literal_marker" "$source_file" || fail "$invariant missing from $relative_path"
done < "$semantic_markers"

while IFS= read -r package; do
  spec="$agent_root/$package/agent.yaml"
  # The AgentImage compiler owns contract identity verification.  It resolves
  # every declared ID against the canonical registry and checks its exact
  # content hash; package-specific counts would be a stale second authority.

  if ! awk '
    /^capabilities:$/ { inside=1; next }
    /^workflows:$/ { inside=0 }
    inside && /output_contracts:/ && $0 !~ /normalized-capability-result\/v1/ { bad=1 }
    END { exit bad }
  ' "$spec"; then
    fail "$package has a capability without normalized-capability-result/v1"
  fi

  # Bash 3.2 treats expansion of an empty array as an unbound variable under
  # `set -u`; keep one skipped sentinel for packages without front bindings.
  expected_binding_keys=("")
  case "$package" in
    krw-feed) expected_binding_keys=("${feed_binding_keys[@]}") ;;
    krw-guru-advisor) expected_binding_keys=("${guru_binding_keys[@]}") ;;
    krw-source-filing) expected_binding_keys=("${source_filing_binding_keys[@]}") ;;
  esac
  for expected_binding_key in "${expected_binding_keys[@]}"; do
    test -n "$expected_binding_key" || continue
    observed_count="$(rg -c "binding_key: ${expected_binding_key}([[:space:]}]|$)" "$spec" || true)"
    # Multiple capabilities may share one physical MCP binding (e.g. a
    # market-wide wrapper over the same tool); require presence, not uniqueness.
    test "$observed_count" -ge 1 || fail "$package must declare binding $expected_binding_key at least once"
  done

  if rg -q '^  - id: skill\.load$' "$spec"; then
    rg -q 'execution: \{ kind: local, builtin: skill_load \}' "$spec" \
      || fail "$package skill.load must be a local builtin"
  fi

  if rg "kind: (plan|assess|compose)" "$spec" | rg -q -v "role_id"; then
    fail "$package has a model-driven state without role_id"
  fi
  if rg "kind: terminal" "$spec" | rg -q -v "terminal: (succeeded|stopped|failed)"; then
    fail "$package has a terminal state without disposition"
  fi
done < <(awk -F '\t' '!/^#/ && NF { print $1 }' "$catalog" | sort -u)

guru_spec="$agent_root/krw-guru-advisor/agent.yaml"
test "$(rg -c '^[[:space:]]+- id: company_evidence_researcher$' "$guru_spec" || true)" -eq 1 \
  || fail "Guru must declare exactly one bounded company evidence role"
if rg -q 'role_id: (company_planner|company_analyst)' "$guru_spec"; then
  fail "Guru must not split or recursively multiply the bounded company evidence role"
fi
if rg -q 'kind: subagent|capability_id: .*subagent|role_id: .*child' "$guru_spec"; then
  fail "Guru bounded in-process role must not expose a recursive child/subagent primitive"
fi
for guru_author in ackman buffett flatt marks terry_smith; do
  test "$(rg -F -c "constants: { fixed_guru_author: ${guru_author} }" "$guru_spec" || true)" -eq 1 \
    || fail "Guru must pin fixed author ${guru_author} to exactly one entrypoint"
done
test "$(rg -F -c 'required_budget_profile: guru_company_advisor' "$guru_spec" || true)" -eq 5 \
  || fail "all five Guru entrypoints must use the bounded Guru budget"

guru_answer_validator="$(sed -n '/- id: grounded_guru_answer/,$p' "$guru_spec")"
for forbidden_identity in \
  '저는 워런 버핏' '제가 워런 버핏' \
  '저는 하워드 막스' '제가 하워드 막스' \
  '저는 빌 애크먼' '제가 빌 애크먼' \
  '저는 브루스 플랫' '제가 브루스 플랫' \
  '저는 테리 스미스' '제가 테리 스미스' \
  '버핏이라면' '막스라면' '애크먼이라면' '플랫이라면' '테리 스미스라면'; do
  printf '%s' "$guru_answer_validator" | rg -F -q -- "$forbidden_identity" \
    || fail "Guru answer validator missing non-impersonation term: $forbidden_identity"
done

guru_budget="$(sed -n '/profile_id: guru_company_advisor/,/profile_id: notebook_transform/p' "$repo_root/deployments/local/budget-registry.yaml")"
for budget_marker in \
  'max_capability_calls: 11' \
  'guru.query_context: 1' \
  'guru.company_brief: 2' \
  'ontology.query_context: 2' \
  'ontology.query: 2' \
  'ontology.trace: 1' \
  'ontology.chain: 1' \
  'guru.review_company_evidence: 2'; do
  printf '%s' "$guru_budget" | rg -F -q -- "$budget_marker" \
    || fail "Guru deployment budget missing exact bound: $budget_marker"
done

deployment_binding="$repo_root/deployments/local/deployment-binding.example.yaml"
for guru_binding_key in \
  krw_guru_query_context \
  krw_guru_company_brief \
  krw_guru_review_company_evidence; do
  test "$(rg -F -c "binding_key: ${guru_binding_key}" "$deployment_binding" || true)" -eq 1 \
    || fail "global deployment must bind ${guru_binding_key} exactly once"
done

if rg -n 'binding_key: (get_coverage_state|list_filing_judgments|get_decision_timeline|list_open_questions)$' \
  "$agent_root"/*/agent.yaml; then
  fail "personal filing capability found in public agent source"
fi

if rg -n -i 'https?://|sk-[a-z0-9]{20,}|deepseek_api_key|anthropic_api_key|endpoint_ref|credential_ref|database_url|supabase' \
  "$agent_root"/*/agent.yaml "$agent_root"/*/prompts; then
  fail "deployment endpoint, credential, provider secret, or database meaning found in agent source"
fi

if rg -n 'verify_evidence' "$agent_root/krw-guru-advisor/agent.yaml" "$agent_root/krw-guru-advisor/prompts"; then
  fail "Guru default path must not invent verify_evidence"
fi

export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$repo_root"
cargo build -q -p krw-agent
agent_bin="$repo_root/target/debug/krw-agent"
image_root="$(mktemp -d "${TMPDIR:-/tmp}/krw-agent-images.XXXXXX")"
trap 'rm -rf -- "$image_root"' EXIT

while IFS= read -r package; do
  "$agent_bin" spec check "$agent_root/$package"
  "$agent_bin" image build "$agent_root/$package" --out "$image_root/$package"
  "$agent_bin" image verify "$image_root/$package"
done < <(awk -F '\t' '!/^#/ && NF { print $1 }' "$catalog" | sort -u)

printf 'all direct-authored agent sources and images verified\n'
