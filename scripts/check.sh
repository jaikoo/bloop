#!/usr/bin/env bash
set -euo pipefail

# Local CI replacement — runs the same checks as the GitHub Actions workflows.
# Usage:
#   ./scripts/check.sh          # run all checks
#   ./scripts/check.sh --quick  # skip tests (fmt + clippy only)
#   ./scripts/check.sh --e2e    # also run E2E tests

QUICK=false
E2E=false

for arg in "$@"; do
  case "$arg" in
    --quick) QUICK=true ;;
    --e2e)   E2E=true ;;
  esac
done

BOLD='\033[1m'
GREEN='\033[0;32m'
RED='\033[0;31m'
RESET='\033[0m'

pass() { echo -e "${GREEN}✓${RESET} $1"; }
fail() { echo -e "${RED}✗${RESET} $1"; exit 1; }
step() { echo -e "\n${BOLD}▸ $1${RESET}"; }

step "Formatting (cargo fmt)"
cargo fmt --all --check 2>&1 || fail "Format check failed. Run: cargo fmt --all"
pass "Format OK"

step "Linting (cargo clippy)"
cargo clippy --all-targets --features "analytics,llm-tracing" 2>&1 || fail "Clippy found issues"
pass "Clippy OK"

if [ "$QUICK" = false ]; then
  step "Tests (cargo test — all features)"
  cargo test --features "analytics,llm-tracing" 2>&1 || fail "Tests failed"
  pass "Tests OK"
fi

if [ "$E2E" = true ]; then
  step "E2E tests (Playwright)"
  cargo build --features llm-tracing
  cd tests/e2e
  npm ci
  npx playwright install chromium
  BLOOP_BIN=../../target/debug/bloop npx playwright test
  cd ../..
  pass "E2E OK"
fi

echo -e "\n${GREEN}${BOLD}All checks passed.${RESET}"
