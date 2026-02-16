#!/usr/bin/env bash
#
# Seed script for Bloop — populates a local instance with realistic error data.
# Usage: ./scripts/seed.sh
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DB="$ROOT/bloop_seed.db"
PORT=5332
BASE="http://127.0.0.1:$PORT"
SERVER_PID=""
SESSION_TOKEN="seed-session-token-for-screenshots"
USER_ID="seed-admin-user-001"

cleanup() {
  if [ -n "$SERVER_PID" ]; then
    echo "Stopping server (PID $SERVER_PID)..."
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  echo "Done."
}
trap cleanup EXIT

# ── 1. Build ──────────────────────────────────────────────────────────
echo "==> Building bloop (with llm-tracing)..."
cargo build --features llm-tracing --manifest-path "$ROOT/Cargo.toml" 2>&1 | tail -1

# ── 2. Clean slate ────────────────────────────────────────────────────
rm -f "$DB" "$DB-wal" "$DB-shm"

# ── 3. Start server ──────────────────────────────────────────────────
echo "==> Starting server on port $PORT..."
BLOOP__DATABASE__PATH="$DB" \
BLOOP__SERVER__PORT="$PORT" \
BLOOP__AUTH__HMAC_SECRET="seed-hmac-secret-for-local-dev-only-32ch" \
  "$ROOT/target/debug/bloop" --config "$ROOT/config.toml" 2>&1 &
SERVER_PID=$!

# Wait for server to be ready
for attempt in $(seq 1 20); do
  if curl -sf "$BASE/health" > /dev/null 2>&1; then
    break
  fi
  sleep 0.5
done

if ! curl -sf "$BASE/health" > /dev/null 2>&1; then
  echo "ERROR: Server failed to start after 10s"
  exit 1
fi
echo "    Server running (PID $SERVER_PID)"

# ── 4. Seed user + session directly in SQLite ─────────────────────────
echo "==> Creating admin user and session..."
NOW=$(date +%s)
EXPIRES=$((NOW + 86400 * 7))

# Sessions are stored as SHA-256 hashes (see src/auth/bearer.rs hash_token)
SESSION_HASH=$(printf '%s' "$SESSION_TOKEN" | openssl dgst -sha256 -hex 2>/dev/null | awk '{print $NF}')

sqlite3 "$DB" "INSERT OR IGNORE INTO webauthn_users (id, username, display_name, created_at, is_admin) VALUES ('$USER_ID', 'admin', 'Bloop Admin', $NOW, 1);"
sqlite3 "$DB" "INSERT OR IGNORE INTO sessions (token, user_id, created_at, expires_at) VALUES ('$SESSION_HASH', '$USER_ID', $NOW, $EXPIRES);"
echo "    User 'admin' created with session token"

# ── 5. Helpers ────────────────────────────────────────────────────────
session_curl() {
  curl -sf -b "bloop_session=$SESSION_TOKEN" "$@"
}

# Send a single event with HMAC auth. Body is read from stdin.
send_event() {
  local api_key="$1"
  local body
  body=$(cat)
  local sig
  sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$api_key" -hex 2>/dev/null | awk '{print $NF}')
  curl -sf -X POST "$BASE/v1/ingest" \
    -H "Content-Type: application/json" \
    -H "X-Forwarded-For: 127.0.0.1" \
    -H "X-Project-Key: $api_key" \
    -H "X-Signature: $sig" \
    -d "$body" > /dev/null
}

# ── 6. Create projects ───────────────────────────────────────────────
echo "==> Creating projects..."

PROJ1=$(session_curl -X POST "$BASE/v1/projects" \
  -H "Content-Type: application/json" \
  -d '{"name":"Acme iOS App","slug":"acme-ios"}')
PROJ1_KEY=$(echo "$PROJ1" | python3 -c "import sys,json; print(json.load(sys.stdin)['api_key'])")
PROJ1_ID=$(echo "$PROJ1" | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])")
echo "    Project 'acme-ios' created (key: ${PROJ1_KEY:0:12}...)"

PROJ2=$(session_curl -X POST "$BASE/v1/projects" \
  -H "Content-Type: application/json" \
  -d '{"name":"Acme Backend API","slug":"acme-api"}')
PROJ2_KEY=$(echo "$PROJ2" | python3 -c "import sys,json; print(json.load(sys.stdin)['api_key'])")
PROJ2_ID=$(echo "$PROJ2" | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])")
echo "    Project 'acme-api' created (key: ${PROJ2_KEY:0:12}...)"

PROJ3=$(session_curl -X POST "$BASE/v1/projects" \
  -H "Content-Type: application/json" \
  -d '{"name":"Acme Android","slug":"acme-android"}')
PROJ3_KEY=$(echo "$PROJ3" | python3 -c "import sys,json; print(json.load(sys.stdin)['api_key'])")
PROJ3_ID=$(echo "$PROJ3" | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])")
echo "    Project 'acme-android' created (key: ${PROJ3_KEY:0:12}...)"

# ── 7. Ingest error events ───────────────────────────────────────────
echo "==> Ingesting error events..."

NOW_MS=$((NOW * 1000))
H=$((3600 * 1000))

# ──── Project 1: Acme iOS App ────────────────────────────────────────

# TypeError — recurring crash on login (12 occurrences → shows aggregation)
for i in $(seq 1 12); do
  TS=$((NOW_MS - (72 - i * 5) * H))
  send_event "$PROJ1_KEY" <<EOF
{"timestamp":$TS,"source":"ios","environment":"production","release":"1.3.0","app_version":"1.3.0","build_number":"4521","route_or_procedure":"/auth/login","screen":"LoginViewController","error_type":"TypeError","message":"Cannot read property accessToken of undefined","stack":"TypeError: Cannot read property accessToken of undefined\n    at LoginViewController.handleLogin (LoginViewController.swift:142)\n    at AuthService.authenticate (AuthService.swift:89)\n    at SessionManager.refresh (SessionManager.swift:34)","user_id_hash":"usr_a1b2","device_id_hash":"dev_iphone_01","metadata":{"os_version":"17.2","device":"iPhone 15 Pro","locale":"en_US"}}
EOF
done
echo "    [acme-ios] 12x TypeError on login"

# NullPointerException on profile screen
for i in $(seq 1 8); do
  TS=$((NOW_MS - (60 - i * 7) * H))
  send_event "$PROJ1_KEY" <<EOF
{"timestamp":$TS,"source":"ios","environment":"production","release":"1.3.0","app_version":"1.3.0","build_number":"4521","route_or_procedure":"/user/profile","screen":"ProfileViewController","error_type":"NullPointerException","message":"Unexpectedly found nil while unwrapping an Optional value","stack":"Fatal error: Unexpectedly found nil\n    at ProfileViewController.loadAvatar (ProfileViewController.swift:78)\n    at ProfileViewController.viewDidLoad (ProfileViewController.swift:23)","user_id_hash":"usr_c3d4","device_id_hash":"dev_iphone_02","metadata":{"os_version":"17.1","device":"iPhone 14","locale":"en_GB"}}
EOF
done
echo "    [acme-ios] 8x NullPointerException on profile"

# NetworkError — intermittent
for i in $(seq 1 6); do
  TS=$((NOW_MS - (48 - i * 6) * H))
  send_event "$PROJ1_KEY" <<EOF
{"timestamp":$TS,"source":"ios","environment":"production","release":"1.2.0","app_version":"1.2.0","build_number":"4400","route_or_procedure":"/api/feed","screen":"FeedViewController","error_type":"NetworkError","message":"The request timed out","stack":"NetworkError: The request timed out\n    at URLSession.dataTask (URLSession.swift:0)\n    at APIClient.fetch (APIClient.swift:67)","user_id_hash":"usr_e5f6","device_id_hash":"dev_ipad_01","metadata":{"os_version":"16.7","device":"iPad Air","network":"cellular"}}
EOF
done
echo "    [acme-ios] 6x NetworkError on feed"

# Staging crash
for i in $(seq 1 3); do
  TS=$((NOW_MS - (24 - i * 4) * H))
  send_event "$PROJ1_KEY" <<EOF
{"timestamp":$TS,"source":"ios","environment":"staging","release":"1.4.0-beta","app_version":"1.4.0-beta","build_number":"4600","route_or_procedure":"/checkout/payment","screen":"PaymentViewController","error_type":"IndexOutOfBoundsException","message":"Index 3 out of range for array of size 2","stack":"IndexOutOfBoundsException: Index 3 out of range\n    at PaymentViewController.selectCard (PaymentViewController.swift:156)","metadata":{"os_version":"17.3","device":"iPhone 15"}}
EOF
done
echo "    [acme-ios] 3x IndexOutOfBounds in staging"

# ──── Project 2: Acme Backend API ────────────────────────────────────

# DatabaseError — pool exhausted
for i in $(seq 1 15); do
  TS=$((NOW_MS - (70 - i * 4) * H))
  send_event "$PROJ2_KEY" <<EOF
{"timestamp":$TS,"source":"api","environment":"production","release":"2.1.0","route_or_procedure":"POST /api/v2/orders","error_type":"DatabaseError","message":"Connection pool exhausted: timeout waiting for available connection","stack":"DatabaseError: Connection pool exhausted\n    at ConnectionPool.acquire (pool.rs:142)\n    at OrderRepository.create (order_repo.rs:56)\n    at OrderService.place_order (order_service.rs:89)","http_status":500,"request_id":"req_db01","metadata":{"db_pool_size":"20","active_connections":"20"}}
EOF
done
echo "    [acme-api] 15x DatabaseError on orders"

# ValidationError
for i in $(seq 1 7); do
  TS=$((NOW_MS - (50 - i * 6) * H))
  send_event "$PROJ2_KEY" <<EOF
{"timestamp":$TS,"source":"api","environment":"production","release":"2.1.0","route_or_procedure":"POST /api/v2/users","error_type":"ValidationError","message":"Invalid email format: missing @ symbol","stack":"ValidationError: Invalid email format\n    at Validator.validate_email (validator.rs:23)\n    at UserService.register (user_service.rs:45)","http_status":422,"request_id":"req_val01","metadata":{"field":"email","constraint":"email_format"}}
EOF
done
echo "    [acme-api] 7x ValidationError on users"

# ExternalServiceError — Stripe
for i in $(seq 1 5); do
  TS=$((NOW_MS - (36 - i * 5) * H))
  send_event "$PROJ2_KEY" <<EOF
{"timestamp":$TS,"source":"api","environment":"production","release":"2.1.0","route_or_procedure":"POST /api/v2/payments/charge","error_type":"ExternalServiceError","message":"Stripe API returned 503: Service temporarily unavailable","stack":"ExternalServiceError: Stripe API 503\n    at StripeClient.charge (stripe.rs:78)\n    at PaymentService.process (payment_service.rs:112)","http_status":503,"request_id":"req_stripe01","metadata":{"provider":"stripe"}}
EOF
done
echo "    [acme-api] 5x ExternalServiceError on payments"

# AuthenticationError
for i in $(seq 1 4); do
  TS=$((NOW_MS - (20 - i * 4) * H))
  send_event "$PROJ2_KEY" <<EOF
{"timestamp":$TS,"source":"api","environment":"production","release":"2.0.5","route_or_procedure":"GET /api/v2/admin/users","error_type":"AuthenticationError","message":"JWT token expired","stack":"AuthenticationError: JWT token expired\n    at JwtMiddleware.verify (jwt.rs:45)\n    at AuthGuard.check (auth_guard.rs:22)","http_status":401,"request_id":"req_auth01","metadata":{"token_age_secs":"7201","max_age_secs":"7200"}}
EOF
done
echo "    [acme-api] 4x AuthenticationError on admin"

# ──── Project 3: Acme Android ────────────────────────────────────────

# OutOfMemoryError
for i in $(seq 1 5); do
  TS=$((NOW_MS - (55 - i * 8) * H))
  send_event "$PROJ3_KEY" <<EOF
{"timestamp":$TS,"source":"android","environment":"production","release":"3.0.1","app_version":"3.0.1","build_number":"892","route_or_procedure":"/gallery","screen":"GalleryActivity","error_type":"OutOfMemoryError","message":"Failed to allocate a 24883216 byte allocation with 4194304 free bytes","stack":"java.lang.OutOfMemoryError: Failed to allocate\n    at android.graphics.BitmapFactory.nativeDecodeStream(BitmapFactory.java:-1)\n    at com.acme.gallery.ImageLoader.loadBitmap(ImageLoader.java:89)","device_id_hash":"dev_android_01","metadata":{"os_version":"14","device":"Pixel 8","heap_mb":"256"}}
EOF
done
echo "    [acme-android] 5x OutOfMemoryError on gallery"

# SecurityException
for i in $(seq 1 4); do
  TS=$((NOW_MS - (40 - i * 8) * H))
  send_event "$PROJ3_KEY" <<EOF
{"timestamp":$TS,"source":"android","environment":"production","release":"3.0.1","app_version":"3.0.1","build_number":"892","route_or_procedure":"/camera","screen":"CameraActivity","error_type":"SecurityException","message":"Permission Denial: requires android.permission.CAMERA","stack":"java.lang.SecurityException: Permission Denial\n    at android.hardware.camera2.CameraManager.openCamera(CameraManager.java:542)\n    at com.acme.camera.CameraHelper.startPreview(CameraHelper.java:67)","device_id_hash":"dev_android_02","metadata":{"os_version":"13","device":"Samsung Galaxy S23","target_sdk":"34"}}
EOF
done
echo "    [acme-android] 4x SecurityException on camera"

# IllegalStateException in staging
for i in $(seq 1 3); do
  TS=$((NOW_MS - (15 - i * 4) * H))
  send_event "$PROJ3_KEY" <<EOF
{"timestamp":$TS,"source":"android","environment":"staging","release":"3.1.0-beta","app_version":"3.1.0-beta","build_number":"910","route_or_procedure":"/settings","screen":"SettingsActivity","error_type":"IllegalStateException","message":"Fragment SettingsFragment not attached to a context","stack":"java.lang.IllegalStateException: Fragment not attached\n    at com.acme.settings.SettingsFragment.savePreferences(SettingsFragment.java:123)","device_id_hash":"dev_android_03","metadata":{"os_version":"14","device":"Pixel 7a"}}
EOF
done
echo "    [acme-android] 3x IllegalStateException in staging"

echo "==> Waiting for pipeline flush..."
sleep 4

# ── 8. Ingest LLM traces ──────────────────────────────────────────────
HMAC_SECRET="seed-hmac-secret-for-local-dev-only-32ch"

send_trace() {
  local path="$1"
  local body
  body=$(cat)
  local sig
  sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$HMAC_SECRET" -hex 2>/dev/null | awk '{print $NF}')
  curl -sf -X POST "$BASE$path" \
    -H "Content-Type: application/json" \
    -H "X-Signature: $sig" \
    -d "$body" > /dev/null
}

send_trace_put() {
  local path="$1"
  local body
  body=$(cat)
  local sig
  sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$HMAC_SECRET" -hex 2>/dev/null | awk '{print $NF}')
  curl -sf -X PUT "$BASE$path" \
    -H "Content-Type: application/json" \
    -H "X-Signature: $sig" \
    -d "$body" > /dev/null
}

echo "==> Ingesting LLM traces..."

# Chat completion traces with GPT-4o (acme-ios project)
for i in $(seq 1 8); do
  TRACE_ID="seed-chat-gpt4o-$i"
  IN_TOK=$((80 + RANDOM % 120))
  OUT_TOK=$((20 + RANDOM % 80))
  COST_MILLI=$((IN_TOK * 5 + OUT_TOK * 15))  # rough gpt-4o pricing in micros/1000
  LATENCY=$((600 + RANDOM % 1800))
  TTFT=$((100 + RANDOM % 400))
  send_trace "/v1/traces" <<EOF
{"id":"$TRACE_ID","name":"chat-completion","session_id":"ios-session-$((i % 3 + 1))","user_id":"user-$((i % 5 + 1))","status":"completed","prompt_name":"chat-assistant","prompt_version":"1.2.0","input":"Help me plan my weekend trip to Portland","output":"Here are some great suggestions for Portland...","spans":[{"id":"span-gpt4o-$i","span_type":"generation","name":"gpt-4o chat","model":"gpt-4o","provider":"openai","input_tokens":$IN_TOK,"output_tokens":$OUT_TOK,"cost":0.$(printf '%04d' $COST_MILLI),"latency_ms":$LATENCY,"time_to_first_token_ms":$TTFT,"status":"ok","input":"Help me plan my weekend trip to Portland","output":"Here are some great suggestions for Portland..."}]}
EOF
done
echo "    [llm] 8x gpt-4o chat-completion traces"

# RAG pipeline traces with Claude (acme-api project)
for i in $(seq 1 6); do
  TRACE_ID="seed-rag-claude-$i"
  IN_TOK=$((200 + RANDOM % 300))
  OUT_TOK=$((100 + RANDOM % 200))
  COST_MILLI=$((IN_TOK * 3 + OUT_TOK * 15))
  LATENCY=$((1500 + RANDOM % 3000))
  TTFT=$((200 + RANDOM % 600))
  RET_LATENCY=$((50 + RANDOM % 150))
  send_trace "/v1/traces" <<EOF
{"id":"$TRACE_ID","name":"rag-pipeline","session_id":"api-session-$((i % 2 + 1))","user_id":"user-$((i % 4 + 1))","status":"completed","prompt_name":"rag-answer","prompt_version":"2.0.1","input":"What is the refund policy for enterprise plans?","output":"Enterprise plans include a 30-day money-back guarantee...","spans":[{"id":"span-ret-$i","span_type":"retrieval","name":"vector-search","latency_ms":$RET_LATENCY,"status":"ok","input":"refund policy enterprise"},{"id":"span-claude-$i","parent_span_id":"span-ret-$i","span_type":"generation","name":"claude-3.5-sonnet answer","model":"claude-3.5-sonnet","provider":"anthropic","input_tokens":$IN_TOK,"output_tokens":$OUT_TOK,"cost":0.$(printf '%04d' $COST_MILLI),"latency_ms":$LATENCY,"time_to_first_token_ms":$TTFT,"status":"ok"}]}
EOF
done
echo "    [llm] 6x claude-3.5-sonnet RAG traces"

# Tool-use agent traces with GPT-4o-mini
for i in $(seq 1 5); do
  TRACE_ID="seed-agent-mini-$i"
  IN_TOK=$((50 + RANDOM % 80))
  OUT_TOK=$((30 + RANDOM % 60))
  COST_MILLI=$((IN_TOK * 1 + OUT_TOK * 3))
  LATENCY=$((400 + RANDOM % 800))
  TOOL_LATENCY=$((100 + RANDOM % 300))
  send_trace "/v1/traces" <<EOF
{"id":"$TRACE_ID","name":"tool-agent","session_id":"api-session-1","user_id":"user-2","status":"completed","prompt_name":"tool-router","prompt_version":"1.0.0","spans":[{"id":"span-mini-gen-$i","span_type":"generation","name":"gpt-4o-mini plan","model":"gpt-4o-mini","provider":"openai","input_tokens":$IN_TOK,"output_tokens":$OUT_TOK,"cost":0.$(printf '%04d' $COST_MILLI),"latency_ms":$LATENCY,"status":"ok"},{"id":"span-mini-tool-$i","parent_span_id":"span-mini-gen-$i","span_type":"tool","name":"web-search","latency_ms":$TOOL_LATENCY,"status":"ok"}]}
EOF
done
echo "    [llm] 5x gpt-4o-mini tool-agent traces"

# Claude-3-haiku fast classification traces
for i in $(seq 1 10); do
  TRACE_ID="seed-classify-haiku-$i"
  IN_TOK=$((20 + RANDOM % 40))
  OUT_TOK=$((5 + RANDOM % 15))
  COST_MILLI=$((IN_TOK * 1 + OUT_TOK * 1))
  LATENCY=$((100 + RANDOM % 300))
  TTFT=$((30 + RANDOM % 80))
  send_trace "/v1/traces" <<EOF
{"id":"$TRACE_ID","name":"intent-classification","session_id":"ios-session-1","user_id":"user-$((i % 5 + 1))","status":"completed","prompt_name":"classifier-v3","prompt_version":"3.1.0","spans":[{"id":"span-haiku-$i","span_type":"generation","name":"claude-3-haiku classify","model":"claude-3-haiku","provider":"anthropic","input_tokens":$IN_TOK,"output_tokens":$OUT_TOK,"cost":0.$(printf '%04d' $COST_MILLI),"latency_ms":$LATENCY,"time_to_first_token_ms":$TTFT,"status":"ok"}]}
EOF
done
echo "    [llm] 10x claude-3-haiku classification traces"

# Error traces (rate limit, timeout)
for i in $(seq 1 3); do
  TRACE_ID="seed-error-ratelimit-$i"
  send_trace "/v1/traces" <<EOF
{"id":"$TRACE_ID","name":"chat-completion","session_id":"ios-session-2","user_id":"user-3","status":"error","spans":[{"id":"span-err-$i","span_type":"generation","name":"gpt-4o call","model":"gpt-4o","provider":"openai","latency_ms":$((200 + RANDOM % 300)),"status":"error","error_message":"Rate limit exceeded: 429 Too Many Requests"}]}
EOF
done
echo "    [llm] 3x error traces (rate limit)"

TRACE_ID="seed-error-timeout-1"
send_trace "/v1/traces" <<EOF
{"id":"$TRACE_ID","name":"rag-pipeline","session_id":"api-session-1","user_id":"user-1","status":"error","spans":[{"id":"span-timeout-1","span_type":"generation","name":"claude-3.5-sonnet answer","model":"claude-3.5-sonnet","provider":"anthropic","latency_ms":30000,"status":"error","error_message":"Request timed out after 30000ms"}]}
EOF
echo "    [llm] 1x error trace (timeout)"

# Running trace (in-progress)
send_trace "/v1/traces" <<EOF
{"id":"seed-running-1","name":"long-analysis","session_id":"api-session-2","user_id":"user-1","status":"running","spans":[{"id":"span-running-1","span_type":"generation","name":"gpt-4o analysis","model":"gpt-4o","provider":"openai","input_tokens":500,"output_tokens":0,"cost":0.0,"latency_ms":0,"status":"ok"}]}
EOF
echo "    [llm] 1x running trace"

echo "==> Waiting for LLM pipeline flush..."
sleep 4

# ── 9. Add LLM scores ────────────────────────────────────────────────
echo "==> Adding LLM trace scores..."

# Score some of the chat-completion traces
QUALITY_VALUES=("0.72" "0.85" "0.91" "0.68" "0.79" "0.88")
for i in $(seq 1 6); do
  IDX=$((i - 1))
  session_curl -X POST "$BASE/v1/llm/traces/seed-chat-gpt4o-$i/scores" \
    -H "Content-Type: application/json" \
    -d "{\"name\":\"quality\",\"value\":${QUALITY_VALUES[$IDX]},\"comment\":\"Auto-scored by QA pipeline\"}" > /dev/null 2>&1 || echo "    (score quality/$i skipped)"
done
echo "    [scores] 6x quality scores on chat traces"

# Score RAG traces for relevance
RELEVANCE_VALUES=("0.82" "0.76" "0.91" "0.88")
for i in $(seq 1 4); do
  IDX=$((i - 1))
  session_curl -X POST "$BASE/v1/llm/traces/seed-rag-claude-$i/scores" \
    -H "Content-Type: application/json" \
    -d "{\"name\":\"relevance\",\"value\":${RELEVANCE_VALUES[$IDX]},\"comment\":\"Retrieval relevance score\"}" > /dev/null 2>&1 || echo "    (score relevance/$i skipped)"
done
echo "    [scores] 4x relevance scores on RAG traces"

# Score some for helpfulness
HELPFUL_VALUES=("0.65" "0.78" "0.52" "0.89")
IDX=0
for i in 1 3 5 7; do
  session_curl -X POST "$BASE/v1/llm/traces/seed-chat-gpt4o-$i/scores" \
    -H "Content-Type: application/json" \
    -d "{\"name\":\"helpfulness\",\"value\":${HELPFUL_VALUES[$IDX]},\"comment\":\"User feedback score\"}" > /dev/null 2>&1 || echo "    (score helpfulness/$i skipped)"
  IDX=$((IDX + 1))
done
echo "    [scores] 4x helpfulness scores"

echo "==> Updating error statuses..."

ERRORS_P1=$(session_curl "$BASE/v1/errors?project_id=$PROJ1_ID")
NETWORK_FP=$(echo "$ERRORS_P1" | python3 -c "
import sys, json
errors = json.load(sys.stdin)
for e in errors:
    if e.get('error_type') == 'NetworkError':
        print(e['fingerprint'])
        break
" 2>/dev/null || true)

if [ -n "$NETWORK_FP" ]; then
  session_curl -X POST "$BASE/v1/errors/$NETWORK_FP/resolve?project_id=$PROJ1_ID" > /dev/null
  echo "    Resolved NetworkError ($NETWORK_FP)"
fi

ERRORS_P2=$(session_curl "$BASE/v1/errors?project_id=$PROJ2_ID")
VALIDATION_FP=$(echo "$ERRORS_P2" | python3 -c "
import sys, json
errors = json.load(sys.stdin)
for e in errors:
    if e.get('error_type') == 'ValidationError':
        print(e['fingerprint'])
        break
" 2>/dev/null || true)

if [ -n "$VALIDATION_FP" ]; then
  session_curl -X POST "$BASE/v1/errors/$VALIDATION_FP/mute?project_id=$PROJ2_ID" > /dev/null
  echo "    Muted ValidationError ($VALIDATION_FP)"
fi

echo "==> Creating alert rules..."
session_curl -X POST "$BASE/v1/alerts" \
  -H "Content-Type: application/json" \
  -d "{\"name\":\"New production errors\",\"config\":{\"type\":\"new_issue\",\"environment\":\"production\"},\"project_id\":\"$PROJ1_ID\",\"description\":\"Alert when a new error fingerprint is first seen in production\",\"environment_filter\":\"production\"}" > /dev/null
echo "    Alert rule 'New production errors' created"

session_curl -X POST "$BASE/v1/alerts" \
  -H "Content-Type: application/json" \
  -d "{\"name\":\"High error rate\",\"config\":{\"type\":\"threshold\",\"threshold\":50,\"window_secs\":3600},\"project_id\":\"$PROJ2_ID\",\"description\":\"Alert when error count exceeds 50 in one hour\",\"source_filter\":\"api\"}" > /dev/null
echo "    Alert rule 'High error rate' created"

# ── Summary ──────────────────────────────────────────────────────────
TOTAL=$(session_curl "$BASE/v1/stats" | python3 -c "import sys,json; print(json.load(sys.stdin).get('total_errors', '?'))" 2>/dev/null || echo "?")
LLM_OVERVIEW=$(curl -sf "$BASE/v1/llm/overview?hours=24" 2>/dev/null || echo "{}")
LLM_TRACES=$(echo "$LLM_OVERVIEW" | python3 -c "import sys,json; print(json.load(sys.stdin).get('total_traces', '?'))" 2>/dev/null || echo "?")
echo ""
echo "==> Seed complete!"
echo "    Total error groups: $TOTAL"
echo "    LLM traces: $LLM_TRACES"
echo "    Projects: acme-ios, acme-api, acme-android"
echo "    Session cookie: bloop_session=$SESSION_TOKEN"
echo ""
echo "    Server still running at $BASE (PID $SERVER_PID)"
echo "    Press Ctrl+C to stop, or run: kill $SERVER_PID"
echo ""

# Keep server running for screenshot capture
wait "$SERVER_PID"
