/**
 * Cross-SDK Integration Test: TypeScript / Node.js
 * Sends LLM traces to a live bloop server and verifies via query API.
 *
 * Requires: BLOOP_TEST_ENDPOINT, BLOOP_TEST_SECRET env vars.
 * Zero dependencies — uses Node.js built-in fetch + crypto.
 */

import { createHmac, randomUUID } from "node:crypto";

const ENDPOINT = process.env.BLOOP_TEST_ENDPOINT || "http://localhost:5332";
const SECRET = process.env.BLOOP_TEST_SECRET || "test-secret";
const LANG = "typescript";
const FLUSH_WAIT = 2500; // ms

function sign(body) {
  return createHmac("sha256", SECRET).update(body).digest("hex");
}

async function post(path, payload) {
  const body = JSON.stringify(payload);
  const sig = sign(body);
  const resp = await fetch(`${ENDPOINT}${path}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-Signature": sig,
    },
    body,
  });
  if (!resp.ok && resp.status !== 401) {
    throw new Error(`POST ${path} failed: ${resp.status} ${await resp.text()}`);
  }
  if (resp.status === 401) return { _status: 401 };
  return resp.json();
}

async function put(path, payload) {
  const body = JSON.stringify(payload);
  const sig = sign(body);
  const resp = await fetch(`${ENDPOINT}${path}`, {
    method: "PUT",
    headers: {
      "Content-Type": "application/json",
      "X-Signature": sig,
    },
    body,
  });
  if (!resp.ok) throw new Error(`PUT ${path} failed: ${resp.status}`);
  return resp.json();
}

async function get(path) {
  const resp = await fetch(`${ENDPOINT}${path}`);
  if (!resp.ok) throw new Error(`GET ${path} failed: ${resp.status}`);
  return resp.json();
}

async function postRawStatus(path, payload, fakeSig) {
  const body = JSON.stringify(payload);
  const resp = await fetch(`${ENDPOINT}${path}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-Signature": fakeSig,
    },
    body,
  });
  return resp.status;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const uid = () => randomUUID().replace(/-/g, "").slice(0, 8);

// ── Tests ──

let passed = 0;
let failed = 0;

async function test(name, fn) {
  try {
    await fn();
    console.log(`  ok - ${name}`);
    passed++;
  } catch (e) {
    console.log(`  FAIL - ${name}: ${e.message}`);
    failed++;
  }
}

function assert(condition, msg) {
  if (!condition) throw new Error(msg || "Assertion failed");
}

function assertEqual(actual, expected, label) {
  if (actual !== expected)
    throw new Error(`${label}: expected ${expected}, got ${actual}`);
}

// ── Test Cases ──

await test("single trace ingest", async () => {
  const traceId = `sdk-test-single-${LANG}-${uid()}`;
  const resp = await post("/v1/traces", {
    id: traceId,
    name: "chat-completion",
    session_id: "test-session",
    user_id: "test-user",
    status: "completed",
    input: "What is 2+2?",
    output: "4",
    spans: [
      {
        id: `span-${uid()}`,
        span_type: "generation",
        name: "gpt-4o call",
        model: "gpt-4o",
        provider: "openai",
        input_tokens: 100,
        output_tokens: 50,
        cost: 0.0025,
        latency_ms: 1200,
        time_to_first_token_ms: 300,
        status: "ok",
        input: "What is 2+2?",
        output: "4",
      },
    ],
  });
  assertEqual(resp.status, "accepted", "ingest status");
  await sleep(FLUSH_WAIT);

  const data = await get(`/v1/llm/traces/${traceId}`);
  assertEqual(data.trace.name, "chat-completion", "trace name");
  assertEqual(data.trace.total_tokens, 150, "total_tokens");
  assertEqual(data.trace.cost_micros, 2500, "cost_micros");
  assertEqual(data.spans.length, 1, "span count");
  assertEqual(data.spans[0].model, "gpt-4o", "span model");
});

await test("batch trace ingest", async () => {
  const ids = [0, 1, 2].map((i) => `sdk-test-batch-${LANG}-${i}-${uid()}`);
  const resp = await post("/v1/traces/batch", {
    traces: [
      { id: ids[0], name: "gen", spans: [{ span_type: "generation", model: "gpt-4o", input_tokens: 50, output_tokens: 25, cost: 0.001 }] },
      { id: ids[1], name: "tool", spans: [{ span_type: "tool", name: "search", latency_ms: 200, status: "ok" }] },
      { id: ids[2], name: "ret", spans: [{ span_type: "retrieval", name: "vector", latency_ms: 50, status: "ok" }] },
    ],
  });
  assertEqual(resp.accepted, 3, "batch accepted");
  assertEqual(resp.dropped, 0, "batch dropped");
});

await test("trace update", async () => {
  const traceId = `sdk-test-update-${LANG}-${uid()}`;
  await post("/v1/traces", {
    id: traceId,
    name: "running-trace",
    status: "running",
    spans: [{ span_type: "generation", model: "gpt-4o", status: "ok" }],
  });
  await sleep(FLUSH_WAIT);

  await put(`/v1/traces/${traceId}`, { status: "completed", output: "Final answer" });
  await sleep(FLUSH_WAIT);

  const data = await get(`/v1/llm/traces/${traceId}`);
  assertEqual(data.trace.status, "completed", "updated status");
});

await test("token & cost accuracy", async () => {
  const traceId = `sdk-test-cost-${LANG}-${uid()}`;
  await post("/v1/traces", {
    id: traceId,
    name: "cost-test",
    status: "completed",
    spans: [
      {
        span_type: "generation",
        model: "claude-3.5-sonnet",
        provider: "anthropic",
        input_tokens: 1000,
        output_tokens: 500,
        cost: 0.012,
        latency_ms: 3000,
        status: "ok",
      },
    ],
  });
  await sleep(FLUSH_WAIT);

  const data = await get(`/v1/llm/traces/${traceId}`);
  assertEqual(data.trace.total_tokens, 1500, "total_tokens");
  assertEqual(data.trace.cost_micros, 12000, "cost_micros");
});

await test("multi-span trace", async () => {
  const traceId = `sdk-test-multi-${LANG}-${uid()}`;
  const parentSpan = `parent-${uid()}`;
  const childSpan = `child-${uid()}`;

  await post("/v1/traces", {
    id: traceId,
    name: "multi-span",
    status: "completed",
    spans: [
      { id: parentSpan, span_type: "generation", model: "gpt-4o", input_tokens: 100, output_tokens: 50, cost: 0.002, latency_ms: 1000, status: "ok" },
      { id: childSpan, parent_span_id: parentSpan, span_type: "tool", name: "fn-call", latency_ms: 200, status: "ok" },
    ],
  });
  await sleep(FLUSH_WAIT);

  const data = await get(`/v1/llm/traces/${traceId}`);
  assertEqual(data.spans.length, 2, "span count");
});

await test("error span", async () => {
  const traceId = `sdk-test-error-${LANG}-${uid()}`;
  await post("/v1/traces", {
    id: traceId,
    name: "error-trace",
    status: "error",
    spans: [{ span_type: "generation", model: "gpt-4o", status: "error", error_message: "Rate limit exceeded", latency_ms: 500 }],
  });
  await sleep(FLUSH_WAIT);

  const data = await get(`/v1/llm/traces/${traceId}`);
  assertEqual(data.trace.status, "error", "trace status");
  assertEqual(data.spans[0].status, "error", "span status");
});

await test("HMAC auth rejection", async () => {
  const status = await postRawStatus(
    "/v1/traces",
    { id: "should-fail", name: "bad-auth", spans: [{ span_type: "generation" }] },
    "0000000000000000000000000000000000000000000000000000000000000000"
  );
  assertEqual(status, 401, "auth rejection status");
});

await test("error tracking still works", async () => {
  const resp = await post("/v1/ingest", {
    timestamp: Math.floor(Date.now() / 1000),
    source: "api",
    environment: "test",
    release: "0.0.1",
    error_type: "TestError",
    message: `SDK harness test ${LANG} ${uid()}`,
  });
  assert(resp.status === "accepted" || JSON.stringify(resp).includes("queued"), `Unexpected: ${JSON.stringify(resp)}`);
});

// ── P3-P6 Endpoint Tests (scores, search, prompts, trace list) ──

await test("prompt version tracking", async () => {
  const traceId = `sdk-test-prompt-${LANG}-${uid()}`;
  const resp = await post("/v1/traces", {
    id: traceId,
    name: "prompt-tracking-test",
    status: "completed",
    prompt_name: "chat-v2",
    prompt_version: "2.1.0",
    spans: [
      {
        span_type: "generation",
        model: "gpt-4o",
        provider: "openai",
        input_tokens: 80,
        output_tokens: 40,
        cost: 0.002,
        latency_ms: 900,
        status: "ok",
      },
    ],
  });
  assertEqual(resp.status, "accepted", "ingest status");
  await sleep(FLUSH_WAIT);

  const data = await get("/v1/llm/prompts?hours=24");
  assert(Array.isArray(data.prompts), "prompts should be an array");
  const entry = data.prompts.find((p) => p.name === "chat-v2");
  assert(entry, "prompt chat-v2 should appear in results");
  assert(entry.total_traces >= 1, "prompt should have at least 1 trace");
});

await test("trace scoring", async () => {
  const traceId = `sdk-test-score-${LANG}-${uid()}`;
  await post("/v1/traces", {
    id: traceId,
    name: "score-test",
    status: "completed",
    spans: [
      {
        span_type: "generation",
        model: "gpt-4o",
        input_tokens: 50,
        output_tokens: 25,
        cost: 0.001,
        latency_ms: 600,
        status: "ok",
      },
    ],
  });
  await sleep(FLUSH_WAIT);

  // POST a score (score endpoints require bearer/session auth; try with HMAC
  // via post() helper — if the server rejects, note as known limitation)
  const scoreResp = await post(`/v1/llm/traces/${traceId}/scores`, {
    name: "quality",
    value: 0.85,
    comment: "Good",
  });
  if (scoreResp._status === 401) {
    // Score endpoints require bearer/session auth, HMAC not accepted
    throw new Error("score POST returned 401 — bearer/session auth required (known limitation)");
  }
  assertEqual(scoreResp.status, "ok", "score post status");

  const scores = await get(`/v1/llm/traces/${traceId}/scores`);
  assert(Array.isArray(scores.scores), "scores should be an array");
  const score = scores.scores.find((s) => s.name === "quality");
  assert(score, "quality score should exist");
  assertEqual(score.value, 0.85, "score value");
});

await test("score summary", async () => {
  // Depends on the scoring test having inserted at least one score
  const data = await get("/v1/llm/scores/summary?hours=24&name=quality");
  assert(data.name === "quality", `expected name=quality, got ${data.name}`);
  assert(data.count >= 1, `expected count >= 1, got ${data.count}`);
  assert(typeof data.avg === "number", "avg should be a number");
});

await test("full-text search", async () => {
  const magic = `magic-word-${uid()}`;
  const traceId = `sdk-test-search-${LANG}-${uid()}`;
  await post("/v1/traces", {
    id: traceId,
    name: `searchable-${magic}`,
    status: "completed",
    spans: [
      {
        span_type: "generation",
        model: "gpt-4o",
        input_tokens: 30,
        output_tokens: 15,
        cost: 0.0005,
        latency_ms: 400,
        status: "ok",
      },
    ],
  });
  await sleep(FLUSH_WAIT);

  const data = await get(`/v1/llm/search?search=${encodeURIComponent(magic)}&hours=24`);
  assert(Array.isArray(data.traces), "search results should have traces array");
  const found = data.traces.find((t) => t.id === traceId);
  assert(found, `trace ${traceId} should appear in search results`);
});

await test("trace list with filters", async () => {
  // Uses traces ingested by previous tests (model=gpt-4o)
  const data = await get("/v1/llm/traces?hours=24&sort=newest");
  assert(Array.isArray(data.traces), "trace list should have traces array");
  assert(data.traces.length >= 1, "should have at least 1 trace");
  assert(typeof data.total === "number", "total should be a number");
});

// ── Summary ──

console.log("");
console.log(`# ${passed} passed, ${failed} failed`);
process.exit(failed > 0 ? 1 : 0);
