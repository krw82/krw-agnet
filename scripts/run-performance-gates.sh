#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PROFILE=${1:-ci}
REPORT_PATH=${2:-"$PROJECT_ROOT/target/perf/$PROFILE-report.json"}

export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

cd "$PROJECT_ROOT"

cargo test -p krw-agent-tool-mcp
cargo test -p krw-agent-run-engine
cargo run --release -p krw-agent-perf-harness -- \
  --profile "$PROFILE" \
  --root "$PROJECT_ROOT" \
  --thresholds "$PROJECT_ROOT/perf/release-gates.json" \
  --output "$REPORT_PATH"
