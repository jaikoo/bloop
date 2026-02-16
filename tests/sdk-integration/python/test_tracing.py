#!/usr/bin/env python3
"""
Cross-SDK Integration Test: Python
Sends LLM traces to a live bloop server and verifies via query API.

Requires: BLOOP_TEST_ENDPOINT, BLOOP_TEST_SECRET env vars.
Zero external dependencies — stdlib only (same philosophy as bloop-sdk).
"""

import hashlib
import hmac
import json
import os
import sys
import time
import urllib.parse
import urllib.request
import urllib.error
import uuid

ENDPOINT = os.environ.get("BLOOP_TEST_ENDPOINT", "http://localhost:5332")
SECRET = os.environ.get("BLOOP_TEST_SECRET", "test-secret")
LANG = "python"
FLUSH_WAIT = 2.5  # seconds to wait for pipeline flush


def sign(body: bytes) -> str:
    """HMAC-SHA256 sign body bytes with the test secret."""
    return hmac.new(SECRET.encode(), body, hashlib.sha256).hexdigest()


def post(path: str, payload: dict) -> dict:
    """POST JSON to bloop with HMAC auth. Returns parsed response."""
    body = json.dumps(payload).encode()
    sig = sign(body)
    req = urllib.request.Request(
        f"{ENDPOINT}{path}",
        data=body,
        headers={
            "Content-Type": "application/json",
            "X-Signature": sig,
        },
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


def put(path: str, payload: dict) -> dict:
    """PUT JSON to bloop with HMAC auth."""
    body = json.dumps(payload).encode()
    sig = sign(body)
    req = urllib.request.Request(
        f"{ENDPOINT}{path}",
        data=body,
        method="PUT",
        headers={
            "Content-Type": "application/json",
            "X-Signature": sig,
        },
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


def get(path: str) -> dict:
    """GET from bloop (no auth — test server has no session requirement for query)."""
    req = urllib.request.Request(f"{ENDPOINT}{path}")
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


def post_raw_status(path: str, payload: dict, sig: str) -> int:
    """POST with custom signature, return HTTP status code."""
    body = json.dumps(payload).encode()
    req = urllib.request.Request(
        f"{ENDPOINT}{path}",
        data=body,
        headers={
            "Content-Type": "application/json",
            "X-Signature": sig,
        },
    )
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status
    except urllib.error.HTTPError as e:
        return e.code


def wait_flush():
    """Wait for pipeline to flush."""
    time.sleep(FLUSH_WAIT)


# ── Test Helpers ──

passed = 0
failed = 0


def test(name: str, fn):
    global passed, failed
    try:
        fn()
        print(f"  ok - {name}")
        passed += 1
    except Exception as e:
        print(f"  FAIL - {name}: {e}")
        failed += 1


# ── Tests ──

def test_single_trace():
    trace_id = f"sdk-test-single-{LANG}-{uuid.uuid4().hex[:8]}"
    resp = post("/v1/traces", {
        "id": trace_id,
        "name": "chat-completion",
        "session_id": "test-session",
        "user_id": "test-user",
        "status": "completed",
        "input": "What is 2+2?",
        "output": "4",
        "spans": [{
            "id": f"span-{uuid.uuid4().hex[:8]}",
            "span_type": "generation",
            "name": "gpt-4o call",
            "model": "gpt-4o",
            "provider": "openai",
            "input_tokens": 100,
            "output_tokens": 50,
            "cost": 0.0025,
            "latency_ms": 1200,
            "time_to_first_token_ms": 300,
            "status": "ok",
            "input": "What is 2+2?",
            "output": "4",
        }],
    })
    assert resp["status"] == "accepted", f"Expected accepted, got {resp}"
    wait_flush()

    data = get(f"/v1/llm/traces/{trace_id}")
    assert data["trace"]["name"] == "chat-completion"
    assert data["trace"]["total_tokens"] == 150
    assert data["trace"]["cost_micros"] == 2500
    assert len(data["spans"]) == 1
    assert data["spans"][0]["model"] == "gpt-4o"


def test_batch_traces():
    ids = [f"sdk-test-batch-{LANG}-{i}-{uuid.uuid4().hex[:8]}" for i in range(3)]
    resp = post("/v1/traces/batch", {
        "traces": [
            {
                "id": ids[0],
                "name": "generation-trace",
                "spans": [{"span_type": "generation", "model": "gpt-4o", "input_tokens": 50, "output_tokens": 25, "cost": 0.001}],
            },
            {
                "id": ids[1],
                "name": "tool-trace",
                "spans": [{"span_type": "tool", "name": "web-search", "latency_ms": 200, "status": "ok"}],
            },
            {
                "id": ids[2],
                "name": "retrieval-trace",
                "spans": [{"span_type": "retrieval", "name": "vector-search", "latency_ms": 50, "status": "ok"}],
            },
        ],
    })
    assert resp["accepted"] == 3, f"Expected 3 accepted, got {resp}"
    assert resp["dropped"] == 0


def test_trace_update():
    trace_id = f"sdk-test-update-{LANG}-{uuid.uuid4().hex[:8]}"
    post("/v1/traces", {
        "id": trace_id,
        "name": "running-trace",
        "status": "running",
        "spans": [{"span_type": "generation", "model": "gpt-4o", "status": "ok"}],
    })
    wait_flush()

    put(f"/v1/traces/{trace_id}", {
        "status": "completed",
        "output": "Final answer",
    })
    wait_flush()

    data = get(f"/v1/llm/traces/{trace_id}")
    assert data["trace"]["status"] == "completed", f"Expected completed, got {data['trace']['status']}"


def test_token_cost_accuracy():
    trace_id = f"sdk-test-cost-{LANG}-{uuid.uuid4().hex[:8]}"
    post("/v1/traces", {
        "id": trace_id,
        "name": "cost-test",
        "status": "completed",
        "spans": [{
            "span_type": "generation",
            "model": "claude-3.5-sonnet",
            "provider": "anthropic",
            "input_tokens": 1000,
            "output_tokens": 500,
            "cost": 0.012,  # $0.012 = 12000 microdollars
            "latency_ms": 3000,
            "status": "ok",
        }],
    })
    wait_flush()

    data = get(f"/v1/llm/traces/{trace_id}")
    assert data["trace"]["total_tokens"] == 1500, f"tokens: {data['trace']['total_tokens']}"
    assert data["trace"]["cost_micros"] == 12000, f"cost_micros: {data['trace']['cost_micros']}"


def test_multi_span():
    trace_id = f"sdk-test-multi-{LANG}-{uuid.uuid4().hex[:8]}"
    parent_span = f"parent-{uuid.uuid4().hex[:8]}"
    child_span = f"child-{uuid.uuid4().hex[:8]}"

    post("/v1/traces", {
        "id": trace_id,
        "name": "multi-span-trace",
        "status": "completed",
        "spans": [
            {
                "id": parent_span,
                "span_type": "generation",
                "model": "gpt-4o",
                "input_tokens": 100,
                "output_tokens": 50,
                "cost": 0.002,
                "latency_ms": 1000,
                "status": "ok",
            },
            {
                "id": child_span,
                "parent_span_id": parent_span,
                "span_type": "tool",
                "name": "function-call",
                "latency_ms": 200,
                "status": "ok",
            },
        ],
    })
    wait_flush()

    data = get(f"/v1/llm/traces/{trace_id}")
    assert len(data["spans"]) == 2, f"Expected 2 spans, got {len(data['spans'])}"


def test_error_span():
    trace_id = f"sdk-test-error-{LANG}-{uuid.uuid4().hex[:8]}"
    post("/v1/traces", {
        "id": trace_id,
        "name": "error-trace",
        "status": "error",
        "spans": [{
            "span_type": "generation",
            "model": "gpt-4o",
            "status": "error",
            "error_message": "Rate limit exceeded",
            "latency_ms": 500,
        }],
    })
    wait_flush()

    data = get(f"/v1/llm/traces/{trace_id}")
    assert data["trace"]["status"] == "error"
    assert data["spans"][0]["status"] == "error"


def test_hmac_auth_rejection():
    status = post_raw_status("/v1/traces", {
        "id": "should-fail",
        "name": "bad-auth",
        "spans": [{"span_type": "generation"}],
    }, sig="0000000000000000000000000000000000000000000000000000000000000000")
    assert status == 401, f"Expected 401, got {status}"


def test_error_tracking():
    """Verify that error tracking still works alongside LLM tracing."""
    body = {
        "timestamp": int(time.time()),
        "source": "api",
        "environment": "test",
        "release": "0.0.1",
        "error_type": "TestError",
        "message": f"SDK harness test {LANG} {uuid.uuid4().hex[:8]}",
    }
    resp = post("/v1/ingest", body)
    assert resp.get("status") == "accepted" or "queued" in str(resp).lower(), f"Unexpected: {resp}"


# ── P3-P6 Endpoint Tests (scores, search, prompts, trace list) ──

def test_prompt_version_tracking():
    trace_id = f"sdk-test-prompt-{LANG}-{uuid.uuid4().hex[:8]}"
    resp = post("/v1/traces", {
        "id": trace_id,
        "name": "prompt-tracking-test",
        "status": "completed",
        "prompt_name": "chat-v2",
        "prompt_version": "2.1.0",
        "spans": [{
            "span_type": "generation",
            "model": "gpt-4o",
            "provider": "openai",
            "input_tokens": 80,
            "output_tokens": 40,
            "cost": 0.002,
            "latency_ms": 900,
            "status": "ok",
        }],
    })
    assert resp["status"] == "accepted", f"Expected accepted, got {resp}"
    wait_flush()

    data = get("/v1/llm/prompts?hours=24")
    assert isinstance(data["prompts"], list), "prompts should be a list"
    entry = next((p for p in data["prompts"] if p["name"] == "chat-v2"), None)
    assert entry is not None, "prompt chat-v2 should appear in results"
    assert entry["total_traces"] >= 1, "prompt should have at least 1 trace"


def test_trace_scoring():
    trace_id = f"sdk-test-score-{LANG}-{uuid.uuid4().hex[:8]}"
    post("/v1/traces", {
        "id": trace_id,
        "name": "score-test",
        "status": "completed",
        "spans": [{
            "span_type": "generation",
            "model": "gpt-4o",
            "input_tokens": 50,
            "output_tokens": 25,
            "cost": 0.001,
            "latency_ms": 600,
            "status": "ok",
        }],
    })
    wait_flush()

    # POST a score (score endpoints require bearer/session auth; try with HMAC
    # via post() helper -- if the server rejects with 401, note as known limitation)
    try:
        score_resp = post(f"/v1/llm/traces/{trace_id}/scores", {
            "name": "quality",
            "value": 0.85,
            "comment": "Good",
        })
    except urllib.error.HTTPError as e:
        if e.code == 401:
            raise AssertionError("score POST returned 401 -- bearer/session auth required (known limitation)")
        raise
    assert score_resp.get("status") == "ok", f"Expected ok, got {score_resp}"

    scores = get(f"/v1/llm/traces/{trace_id}/scores")
    assert isinstance(scores["scores"], list), "scores should be a list"
    score = next((s for s in scores["scores"] if s["name"] == "quality"), None)
    assert score is not None, "quality score should exist"
    assert score["value"] == 0.85, f"Expected 0.85, got {score['value']}"


def test_score_summary():
    # Depends on the scoring test having inserted at least one score
    data = get("/v1/llm/scores/summary?hours=24&name=quality")
    assert data["name"] == "quality", f"Expected name=quality, got {data['name']}"
    assert data["count"] >= 1, f"Expected count >= 1, got {data['count']}"
    assert isinstance(data["avg"], (int, float)), "avg should be a number"


def test_full_text_search():
    magic = f"magic-word-{uuid.uuid4().hex[:8]}"
    trace_id = f"sdk-test-search-{LANG}-{uuid.uuid4().hex[:8]}"
    post("/v1/traces", {
        "id": trace_id,
        "name": f"searchable-{magic}",
        "status": "completed",
        "spans": [{
            "span_type": "generation",
            "model": "gpt-4o",
            "input_tokens": 30,
            "output_tokens": 15,
            "cost": 0.0005,
            "latency_ms": 400,
            "status": "ok",
        }],
    })
    wait_flush()

    encoded = urllib.parse.quote(magic)
    data = get(f"/v1/llm/search?search={encoded}&hours=24")
    assert isinstance(data["traces"], list), "search results should have traces list"
    found = next((t for t in data["traces"] if t["id"] == trace_id), None)
    assert found is not None, f"trace {trace_id} should appear in search results"


def test_trace_list_with_filters():
    # Uses traces ingested by previous tests (model=gpt-4o)
    data = get("/v1/llm/traces?hours=24&sort=newest")
    assert isinstance(data["traces"], list), "trace list should have traces list"
    assert len(data["traces"]) >= 1, "should have at least 1 trace"
    assert isinstance(data["total"], int), "total should be an integer"


# ── Run ──

if __name__ == "__main__":
    print(f"TAP version 13")
    print(f"# Python SDK Integration Tests")
    print(f"# endpoint={ENDPOINT}")
    print(f"1..13")

    test("single trace ingest", test_single_trace)
    test("batch trace ingest", test_batch_traces)
    test("trace update", test_trace_update)
    test("token & cost accuracy", test_token_cost_accuracy)
    test("multi-span trace", test_multi_span)
    test("error span", test_error_span)
    test("HMAC auth rejection", test_hmac_auth_rejection)
    test("error tracking still works", test_error_tracking)
    test("prompt version tracking", test_prompt_version_tracking)
    test("trace scoring", test_trace_scoring)
    test("score summary", test_score_summary)
    test("full-text search", test_full_text_search)
    test("trace list with filters", test_trace_list_with_filters)

    print(f"")
    print(f"# {passed} passed, {failed} failed")
    sys.exit(1 if failed > 0 else 0)
