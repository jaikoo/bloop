#!/bin/bash
# Benchmark script for bloop
# Generates a signed payload and uses hey for load testing

set -e

HOST="${1:-http://localhost:3939}"
SECRET="${BLOOP_SECRET:-change-me-in-production}"
N="${2:-5000}"
C="${3:-50}"

# Single event payload
SINGLE_BODY='{"timestamp":1700000000000,"source":"api","environment":"prod","release":"bench-1.0","error_type":"TypeError","message":"Cannot read property id of undefined","route_or_procedure":"/api/users"}'
SINGLE_SIG=$(echo -n "$SINGLE_BODY" | openssl dgst -sha256 -hmac "$SECRET" | awk '{print $NF}')

# Batch payload (10 events)
BATCH_BODY='{"events":['
for i in $(seq 1 10); do
  [ $i -gt 1 ] && BATCH_BODY+=','
  BATCH_BODY+="{\"timestamp\":$((1700000000000 + i)),\"source\":\"api\",\"environment\":\"prod\",\"release\":\"bench-1.0\",\"error_type\":\"Error$i\",\"message\":\"Benchmark error number $i\",\"route_or_procedure\":\"/api/route$i\"}"
done
BATCH_BODY+=']}'
BATCH_SIG=$(echo -n "$BATCH_BODY" | openssl dgst -sha256 -hmac "$SECRET" | awk '{print $NF}')

echo -n "$SINGLE_BODY" > /tmp/bloop-single.json
echo -n "$BATCH_BODY" > /tmp/bloop-batch.json

echo "=== SINGLE EVENT: $N requests, $C concurrent ==="
hey -n "$N" -c "$C" \
  -m POST \
  -H "Content-Type: application/json" \
  -H "X-Signature: $SINGLE_SIG" \
  -D /tmp/bloop-single.json \
  "$HOST/v1/ingest"

echo ""
echo "=== BATCH (10 events/req): $N requests, $C concurrent ==="
echo "=== Effective event rate = requests/sec * 10 ==="
hey -n "$N" -c "$C" \
  -m POST \
  -H "Content-Type: application/json" \
  -H "X-Signature: $BATCH_SIG" \
  -D /tmp/bloop-batch.json \
  "$HOST/v1/ingest/batch"
