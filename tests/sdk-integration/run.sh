#!/usr/bin/env bash
# Cross-SDK LLM Tracing Integration Test Harness
#
# Builds bloop with llm-tracing, starts it on a random port, then runs
# each SDK's integration test against it. Cleans up on exit.
#
# Usage:
#   ./run.sh              # Run all SDK tests
#   ./run.sh rust python  # Run only specified SDKs
#
# Env overrides:
#   BLOOP_BIN       — path to pre-built bloop binary (skip cargo build)
#   BLOOP_PORT      — fixed port (default: auto-detect from health check)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BLOOP_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
HMAC_SECRET="test-harness-secret-$(date +%s)"
BLOOP_PORT="${BLOOP_PORT:-5332}"
TMPDB=""
BLOOP_PID=""

cleanup() {
  echo ""
  echo "=== Cleanup ==="
  if [ -n "$BLOOP_PID" ]; then
    kill "$BLOOP_PID" 2>/dev/null || true
    wait "$BLOOP_PID" 2>/dev/null || true
  fi
  if [ -n "$TMPDB" ]; then
    rm -f "$TMPDB" "${TMPDB}-wal" "${TMPDB}-shm"
  fi
}
trap cleanup EXIT

# ── Build ──
BLOOP_BIN="${BLOOP_BIN:-}"
if [ -z "$BLOOP_BIN" ]; then
  echo "=== Building bloop with llm-tracing ==="
  cd "$BLOOP_ROOT"
  cargo build --features "llm-tracing" 2>&1
  BLOOP_BIN="$BLOOP_ROOT/target/debug/bloop"
fi

if [ ! -x "$BLOOP_BIN" ]; then
  echo "ERROR: bloop binary not found at $BLOOP_BIN"
  exit 1
fi

# ── Start server ──
echo "=== Starting bloop server on port $BLOOP_PORT ==="
TMPDB=$(mktemp /tmp/bloop-test-XXXXXX.db)

BLOOP__DATABASE__PATH="$TMPDB" \
BLOOP__AUTH__HMAC_SECRET="$HMAC_SECRET" \
BLOOP__SERVER__PORT="$BLOOP_PORT" \
BLOOP__LLM_TRACING__ENABLED=true \
BLOOP__LLM_TRACING__DEFAULT_CONTENT_STORAGE=full \
BLOOP__LLM_TRACING__FLUSH_INTERVAL_SECS=1 \
BLOOP__LLM_TRACING__FLUSH_BATCH_SIZE=50 \
BLOOP__PIPELINE__FLUSH_INTERVAL_SECS=1 \
BLOOP__RETENTION__PRUNE_INTERVAL_SECS=999999 \
RUST_LOG=bloop=warn \
  "$BLOOP_BIN" &
BLOOP_PID=$!

# Wait for health check
echo -n "Waiting for server"
for i in $(seq 1 60); do
  if curl -sf "http://localhost:${BLOOP_PORT}/health" > /dev/null 2>&1; then
    echo " ready!"
    break
  fi
  if ! kill -0 "$BLOOP_PID" 2>/dev/null; then
    echo ""
    echo "ERROR: bloop process died during startup"
    exit 1
  fi
  echo -n "."
  sleep 0.3
done

if ! curl -sf "http://localhost:${BLOOP_PORT}/health" > /dev/null 2>&1; then
  echo ""
  echo "ERROR: bloop failed to start within 18s"
  exit 1
fi

export BLOOP_TEST_ENDPOINT="http://localhost:${BLOOP_PORT}"
export BLOOP_TEST_SECRET="$HMAC_SECRET"

# ── Determine which SDKs to test ──
if [ $# -gt 0 ]; then
  SDKS=("$@")
else
  SDKS=()
  [ -d "$SCRIPT_DIR/rust" ]       && SDKS+=(rust)
  [ -d "$SCRIPT_DIR/typescript" ] && SDKS+=(typescript)
  [ -d "$SCRIPT_DIR/python" ]     && SDKS+=(python)
  [ -d "$SCRIPT_DIR/ruby" ]       && SDKS+=(ruby)
fi

# ── Run tests ──
PASSED=0
FAILED=0
SKIPPED=0

run_test() {
  local name="$1"
  local dir="$SCRIPT_DIR/$name"
  local cmd=""

  if [ ! -d "$dir" ]; then
    echo ""
    echo "--- SKIP: $name (directory not found) ---"
    SKIPPED=$((SKIPPED + 1))
    return
  fi

  case "$name" in
    rust)
      if ! command -v cargo &>/dev/null; then
        echo "--- SKIP: rust (cargo not found) ---"
        SKIPPED=$((SKIPPED + 1))
        return
      fi
      cmd="cd '$dir' && cargo run --quiet 2>&1"
      ;;
    typescript)
      if ! command -v node &>/dev/null; then
        echo "--- SKIP: typescript (node not found) ---"
        SKIPPED=$((SKIPPED + 1))
        return
      fi
      cmd="cd '$dir' && npm install --silent 2>/dev/null && node test.mjs 2>&1"
      ;;
    python)
      if ! command -v python3 &>/dev/null; then
        echo "--- SKIP: python (python3 not found) ---"
        SKIPPED=$((SKIPPED + 1))
        return
      fi
      cmd="cd '$dir' && python3 test_tracing.py 2>&1"
      ;;
    ruby)
      if ! command -v ruby &>/dev/null; then
        echo "--- SKIP: ruby (ruby not found) ---"
        SKIPPED=$((SKIPPED + 1))
        return
      fi
      cmd="cd '$dir' && ruby test_tracing.rb 2>&1"
      ;;
    *)
      echo "--- SKIP: $name (unknown SDK) ---"
      SKIPPED=$((SKIPPED + 1))
      return
      ;;
  esac

  echo ""
  echo "=== Testing $name SDK ==="
  if eval "$cmd"; then
    echo "PASS: $name"
    PASSED=$((PASSED + 1))
  else
    echo "FAIL: $name"
    FAILED=$((FAILED + 1))
  fi
}

for sdk in "${SDKS[@]}"; do
  run_test "$sdk"
done

# ── Summary ──
echo ""
echo "=========================================="
echo "  Results: $PASSED passed, $FAILED failed, $SKIPPED skipped"
echo "=========================================="

if [ $FAILED -gt 0 ]; then
  exit 1
fi
exit 0
