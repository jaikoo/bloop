#!/bin/bash
# Test script for bloop LLM Proxy
# This script tests the proxy endpoint with mock OpenAI requests

set -e

BLOOP_URL="http://localhost:5332"
OPENAI_API_KEY="${OPENAI_API_KEY:-sk-test-key}"

echo "=== bloop LLM Proxy Test Script ==="
echo ""

# Check if bloop server is running
echo "1. Checking if bloop server is running..."
if curl -s "$BLOOP_URL/health" > /dev/null 2>&1; then
    echo "   ✓ Server is running"
else
    echo "   ✗ Server is not running. Please start it first:"
    echo "     cargo run --features llm-tracing"
    exit 1
fi

# Test 1: Non-streaming proxy request
echo ""
echo "2. Testing non-streaming proxy request..."
RESPONSE=$(curl -s -X POST "$BLOOP_URL/v1/proxy/openai/chat/completions" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -d '{
    "model": "gpt-4o-mini",
    "messages": [{"role": "user", "content": "Say hello"}],
    "stream": false
  }' 2>&1 || echo "ERROR")

if echo "$RESPONSE" | grep -q "error\|ERROR"; then
    echo "   ✗ Request failed:"
    echo "$RESPONSE" | head -20
else
    echo "   ✓ Request succeeded (forwarded to OpenAI)"
    echo "   Response preview:"
    echo "$RESPONSE" | head -5
fi

# Test 2: Streaming proxy request
echo ""
echo "3. Testing streaming proxy request..."
STREAM_RESPONSE=$(curl -s -X POST "$BLOOP_URL/v1/proxy/openai/chat/completions" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -d '{
    "model": "gpt-4o-mini",
    "messages": [{"role": "user", "content": "Count to 3"}],
    "stream": true
  }' 2>&1 | head -10 || echo "ERROR")

if echo "$STREAM_RESPONSE" | grep -q "error\|ERROR"; then
    echo "   ✗ Streaming request failed"
else
    echo "   ✓ Streaming request succeeded"
    echo "   First few chunks:"
    echo "$STREAM_RESPONSE"
fi

# Test 3: Check traces were recorded
echo ""
echo "4. Checking if traces were recorded..."
# Wait a moment for async processing
sleep 2

# Query traces via API (requires auth token)
if [ -n "$BLOOP_API_TOKEN" ]; then
    TRACES=$(curl -s "$BLOOP_URL/v1/llm/traces?limit=5" \
      -H "Authorization: Bearer $BLOOP_API_TOKEN" 2>&1 || echo "ERROR")
    
    if echo "$TRACES" | grep -q "traces"; then
        echo "   ✓ Traces recorded successfully"
        echo "$TRACES" | head -20
    else
        echo "   ⚠ Could not verify traces (check API token)"
    fi
else
    echo "   ℹ Set BLOOP_API_TOKEN to verify trace recording"
fi

# Test 4: Export to LangSmith format
echo ""
echo "5. Testing LangSmith export endpoint..."
if [ -n "$BLOOP_API_TOKEN" ]; then
    EXPORT=$(curl -s "$BLOOP_URL/v1/llm/export/langsmith?hours=1" \
      -H "Authorization: Bearer $BLOOP_API_TOKEN" 2>&1 || echo "ERROR")
    
    if echo "$EXPORT" | grep -q "runs"; then
        echo "   ✓ LangSmith export working"
        echo "$EXPORT" | head -10
    else
        echo "   ✗ Export failed:"
        echo "$EXPORT" | head -20
    fi
else
    echo "   ℹ Set BLOOP_API_TOKEN to test export"
fi

echo ""
echo "=== Test Summary ==="
echo "The proxy is working if steps 2-3 show successful forwarding."
echo "Traces are recorded asynchronously and may take a few seconds to appear."
echo ""
echo "Next steps:"
echo "1. Configure your app to use the proxy:"
echo "   export OPENAI_BASE_URL=http://localhost:5332/v1/proxy/openai"
echo "2. Make some LLM calls through your app"
echo "3. Check traces at: http://localhost:5332/v1/llm/traces"
