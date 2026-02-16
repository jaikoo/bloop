#!/usr/bin/env ruby
# Cross-SDK Integration Test: Ruby
# Sends LLM traces to a live bloop server and verifies via query API.
#
# Requires: BLOOP_TEST_ENDPOINT, BLOOP_TEST_SECRET env vars.
# Zero external dependencies — stdlib only (same philosophy as bloop gem).

require "openssl"
require "json"
require "net/http"
require "uri"
require "securerandom"

ENDPOINT = ENV.fetch("BLOOP_TEST_ENDPOINT", "http://localhost:5332")
SECRET = ENV.fetch("BLOOP_TEST_SECRET", "test-secret")
LANG = "ruby"
FLUSH_WAIT = 2.5 # seconds

def sign(body)
  OpenSSL::HMAC.hexdigest("SHA256", SECRET, body)
end

def post(path, payload)
  uri = URI("#{ENDPOINT}#{path}")
  body = JSON.generate(payload)
  sig = sign(body)

  req = Net::HTTP::Post.new(uri)
  req["Content-Type"] = "application/json"
  req["X-Signature"] = sig
  req.body = body

  resp = Net::HTTP.start(uri.hostname, uri.port) { |http| http.request(req) }
  JSON.parse(resp.body)
end

def put(path, payload)
  uri = URI("#{ENDPOINT}#{path}")
  body = JSON.generate(payload)
  sig = sign(body)

  req = Net::HTTP::Put.new(uri)
  req["Content-Type"] = "application/json"
  req["X-Signature"] = sig
  req.body = body

  resp = Net::HTTP.start(uri.hostname, uri.port) { |http| http.request(req) }
  JSON.parse(resp.body)
end

def get(path)
  uri = URI("#{ENDPOINT}#{path}")
  resp = Net::HTTP.get_response(uri)
  JSON.parse(resp.body)
end

def post_raw_status(path, payload, fake_sig)
  uri = URI("#{ENDPOINT}#{path}")
  body = JSON.generate(payload)

  req = Net::HTTP::Post.new(uri)
  req["Content-Type"] = "application/json"
  req["X-Signature"] = fake_sig
  req.body = body

  resp = Net::HTTP.start(uri.hostname, uri.port) { |http| http.request(req) }
  resp.code.to_i
end

def uid
  SecureRandom.hex(4)
end

def wait_flush
  sleep(FLUSH_WAIT)
end

# ── Test Runner ──

$passed = 0
$failed = 0

def test(name)
  yield
  puts "  ok - #{name}"
  $passed += 1
rescue => e
  puts "  FAIL - #{name}: #{e.message}"
  $failed += 1
end

def assert_equal(actual, expected, label)
  raise "#{label}: expected #{expected.inspect}, got #{actual.inspect}" unless actual == expected
end

def assert(condition, msg = "Assertion failed")
  raise msg unless condition
end

# ── Tests ──

puts "TAP version 13"
puts "# Ruby SDK Integration Tests"
puts "# endpoint=#{ENDPOINT}"
puts "1..13"

test "single trace ingest" do
  trace_id = "sdk-test-single-#{LANG}-#{uid}"
  resp = post("/v1/traces", {
    id: trace_id,
    name: "chat-completion",
    session_id: "test-session",
    user_id: "test-user",
    status: "completed",
    input: "What is 2+2?",
    output: "4",
    spans: [{
      id: "span-#{uid}",
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
    }],
  })
  assert_equal resp["status"], "accepted", "ingest status"
  wait_flush

  data = get("/v1/llm/traces/#{trace_id}")
  assert_equal data["trace"]["name"], "chat-completion", "trace name"
  assert_equal data["trace"]["total_tokens"], 150, "total_tokens"
  assert_equal data["trace"]["cost_micros"], 2500, "cost_micros"
  assert_equal data["spans"].length, 1, "span count"
  assert_equal data["spans"][0]["model"], "gpt-4o", "span model"
end

test "batch trace ingest" do
  ids = 3.times.map { |i| "sdk-test-batch-#{LANG}-#{i}-#{uid}" }
  resp = post("/v1/traces/batch", {
    traces: [
      { id: ids[0], name: "gen", spans: [{ span_type: "generation", model: "gpt-4o", input_tokens: 50, output_tokens: 25, cost: 0.001 }] },
      { id: ids[1], name: "tool", spans: [{ span_type: "tool", name: "search", latency_ms: 200, status: "ok" }] },
      { id: ids[2], name: "ret", spans: [{ span_type: "retrieval", name: "vector", latency_ms: 50, status: "ok" }] },
    ],
  })
  assert_equal resp["accepted"], 3, "batch accepted"
  assert_equal resp["dropped"], 0, "batch dropped"
end

test "trace update" do
  trace_id = "sdk-test-update-#{LANG}-#{uid}"
  post("/v1/traces", {
    id: trace_id,
    name: "running-trace",
    status: "running",
    spans: [{ span_type: "generation", model: "gpt-4o", status: "ok" }],
  })
  wait_flush

  put("/v1/traces/#{trace_id}", { status: "completed", output: "Final answer" })
  wait_flush

  data = get("/v1/llm/traces/#{trace_id}")
  assert_equal data["trace"]["status"], "completed", "updated status"
end

test "token & cost accuracy" do
  trace_id = "sdk-test-cost-#{LANG}-#{uid}"
  post("/v1/traces", {
    id: trace_id,
    name: "cost-test",
    status: "completed",
    spans: [{
      span_type: "generation",
      model: "claude-3.5-sonnet",
      provider: "anthropic",
      input_tokens: 1000,
      output_tokens: 500,
      cost: 0.012,
      latency_ms: 3000,
      status: "ok",
    }],
  })
  wait_flush

  data = get("/v1/llm/traces/#{trace_id}")
  assert_equal data["trace"]["total_tokens"], 1500, "total_tokens"
  assert_equal data["trace"]["cost_micros"], 12000, "cost_micros"
end

test "multi-span trace" do
  trace_id = "sdk-test-multi-#{LANG}-#{uid}"
  parent_span = "parent-#{uid}"
  child_span = "child-#{uid}"

  post("/v1/traces", {
    id: trace_id,
    name: "multi-span",
    status: "completed",
    spans: [
      { id: parent_span, span_type: "generation", model: "gpt-4o", input_tokens: 100, output_tokens: 50, cost: 0.002, latency_ms: 1000, status: "ok" },
      { id: child_span, parent_span_id: parent_span, span_type: "tool", name: "fn-call", latency_ms: 200, status: "ok" },
    ],
  })
  wait_flush

  data = get("/v1/llm/traces/#{trace_id}")
  assert_equal data["spans"].length, 2, "span count"
end

test "error span" do
  trace_id = "sdk-test-error-#{LANG}-#{uid}"
  post("/v1/traces", {
    id: trace_id,
    name: "error-trace",
    status: "error",
    spans: [{ span_type: "generation", model: "gpt-4o", status: "error", error_message: "Rate limit exceeded", latency_ms: 500 }],
  })
  wait_flush

  data = get("/v1/llm/traces/#{trace_id}")
  assert_equal data["trace"]["status"], "error", "trace status"
  assert_equal data["spans"][0]["status"], "error", "span status"
end

test "HMAC auth rejection" do
  status = post_raw_status(
    "/v1/traces",
    { id: "should-fail", name: "bad-auth", spans: [{ span_type: "generation" }] },
    "0" * 64
  )
  assert_equal status, 401, "auth rejection status"
end

test "error tracking still works" do
  resp = post("/v1/ingest", {
    timestamp: Time.now.to_i,
    source: "api",
    environment: "test",
    release: "0.0.1",
    error_type: "TestError",
    message: "SDK harness test #{LANG} #{uid}",
  })
  assert(resp["status"] == "accepted" || resp.to_s.include?("queued"), "Unexpected: #{resp}")
end

# ── P3-P6 Endpoint Tests (scores, search, prompts, trace list) ──

test "prompt version tracking" do
  trace_id = "sdk-test-prompt-#{LANG}-#{uid}"
  resp = post("/v1/traces", {
    id: trace_id,
    name: "prompt-tracking-test",
    status: "completed",
    prompt_name: "chat-v2",
    prompt_version: "2.1.0",
    spans: [{
      span_type: "generation",
      model: "gpt-4o",
      provider: "openai",
      input_tokens: 80,
      output_tokens: 40,
      cost: 0.002,
      latency_ms: 900,
      status: "ok",
    }],
  })
  assert_equal resp["status"], "accepted", "ingest status"
  wait_flush

  data = get("/v1/llm/prompts?hours=24")
  assert data["prompts"].is_a?(Array), "prompts should be an array"
  entry = data["prompts"].find { |p| p["name"] == "chat-v2" }
  assert entry, "prompt chat-v2 should appear in results"
  assert entry["total_traces"] >= 1, "prompt should have at least 1 trace"
end

test "trace scoring" do
  trace_id = "sdk-test-score-#{LANG}-#{uid}"
  post("/v1/traces", {
    id: trace_id,
    name: "score-test",
    status: "completed",
    spans: [{
      span_type: "generation",
      model: "gpt-4o",
      input_tokens: 50,
      output_tokens: 25,
      cost: 0.001,
      latency_ms: 600,
      status: "ok",
    }],
  })
  wait_flush

  # POST a score (score endpoints require bearer/session auth; try with HMAC
  # via post() helper -- if the server rejects with 401, note as known limitation)
  begin
    score_resp = post("/v1/llm/traces/#{trace_id}/scores", {
      name: "quality",
      value: 0.85,
      comment: "Good",
    })
  rescue => e
    if e.message.include?("401")
      raise "score POST returned 401 -- bearer/session auth required (known limitation)"
    end
    raise
  end
  assert_equal score_resp["status"], "ok", "score post status"

  scores = get("/v1/llm/traces/#{trace_id}/scores")
  assert scores["scores"].is_a?(Array), "scores should be an array"
  score = scores["scores"].find { |s| s["name"] == "quality" }
  assert score, "quality score should exist"
  assert_equal score["value"], 0.85, "score value"
end

test "score summary" do
  # Depends on the scoring test having inserted at least one score
  data = get("/v1/llm/scores/summary?hours=24&name=quality")
  assert_equal data["name"], "quality", "score summary name"
  assert data["count"] >= 1, "expected count >= 1, got #{data['count']}"
  assert data["avg"].is_a?(Numeric), "avg should be a number"
end

test "full-text search" do
  magic = "magic-word-#{uid}"
  trace_id = "sdk-test-search-#{LANG}-#{uid}"
  post("/v1/traces", {
    id: trace_id,
    name: "searchable-#{magic}",
    status: "completed",
    spans: [{
      span_type: "generation",
      model: "gpt-4o",
      input_tokens: 30,
      output_tokens: 15,
      cost: 0.0005,
      latency_ms: 400,
      status: "ok",
    }],
  })
  wait_flush

  encoded = URI.encode_www_form_component(magic)
  data = get("/v1/llm/search?search=#{encoded}&hours=24")
  assert data["traces"].is_a?(Array), "search results should have traces array"
  found = data["traces"].find { |t| t["id"] == trace_id }
  assert found, "trace #{trace_id} should appear in search results"
end

test "trace list with filters" do
  # Uses traces ingested by previous tests (model=gpt-4o)
  data = get("/v1/llm/traces?hours=24&sort=newest")
  assert data["traces"].is_a?(Array), "trace list should have traces array"
  assert data["traces"].length >= 1, "should have at least 1 trace"
  assert data["total"].is_a?(Integer), "total should be an integer"
end

# ── Summary ──

puts ""
puts "# #{$passed} passed, #{$failed} failed"
exit($failed > 0 ? 1 : 0)
