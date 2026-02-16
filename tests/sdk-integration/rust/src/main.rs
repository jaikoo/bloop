//! Cross-SDK Integration Test: Rust
//!
//! Sends LLM traces to a live bloop server and verifies via query API.
//! Requires: BLOOP_TEST_ENDPOINT, BLOOP_TEST_SECRET env vars.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const LANG: &str = "rust";
const FLUSH_WAIT_MS: u64 = 2500;

fn endpoint() -> String {
    env::var("BLOOP_TEST_ENDPOINT").unwrap_or_else(|_| "http://localhost:5332".to_string())
}

fn secret() -> String {
    env::var("BLOOP_TEST_SECRET").unwrap_or_else(|_| "test-secret".to_string())
}

fn sign(body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret().as_bytes()).unwrap();
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

fn uid() -> String {
    uuid::Uuid::new_v4().to_string().replace('-', "")[..8].to_string()
}

async fn post(client: &reqwest::Client, path: &str, payload: &serde_json::Value) -> serde_json::Value {
    let body = serde_json::to_vec(payload).unwrap();
    let sig = sign(&body);
    let resp = client
        .post(format!("{}{}", endpoint(), path))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "POST {} failed: {}", path, resp.status());
    resp.json().await.unwrap()
}

async fn put(client: &reqwest::Client, path: &str, payload: &serde_json::Value) -> serde_json::Value {
    let body = serde_json::to_vec(payload).unwrap();
    let sig = sign(&body);
    let resp = client
        .put(format!("{}{}", endpoint(), path))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "PUT {} failed: {}", path, resp.status());
    resp.json().await.unwrap()
}

async fn get(client: &reqwest::Client, path: &str) -> serde_json::Value {
    let resp = client
        .get(format!("{}{}", endpoint(), path))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "GET {} failed: {}", path, resp.status());
    resp.json().await.unwrap()
}

async fn post_raw_status(client: &reqwest::Client, path: &str, payload: &serde_json::Value, fake_sig: &str) -> u16 {
    let body = serde_json::to_vec(payload).unwrap();
    let resp = client
        .post(format!("{}{}", endpoint(), path))
        .header("Content-Type", "application/json")
        .header("X-Signature", fake_sig)
        .body(body)
        .send()
        .await
        .unwrap();
    resp.status().as_u16()
}

async fn wait_flush() {
    tokio::time::sleep(std::time::Duration::from_millis(FLUSH_WAIT_MS)).await;
}

struct TestRunner {
    passed: u32,
    failed: u32,
}

impl TestRunner {
    fn new() -> Self { Self { passed: 0, failed: 0 } }

    async fn run<F, Fut>(&mut self, name: &str, f: F)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        match f().await {
            Ok(()) => {
                println!("  ok - {}", name);
                self.passed += 1;
            }
            Err(e) => {
                println!("  FAIL - {}: {}", name, e);
                self.failed += 1;
            }
        }
    }
}

macro_rules! assert_eq_test {
    ($actual:expr, $expected:expr, $label:expr) => {
        if $actual != $expected {
            return Err(format!("{}: expected {:?}, got {:?}", $label, $expected, $actual));
        }
    };
}

#[tokio::main]
async fn main() {
    let client = reqwest::Client::new();
    let mut runner = TestRunner::new();

    println!("TAP version 13");
    println!("# Rust SDK Integration Tests");
    println!("# endpoint={}", endpoint());
    println!("1..8");

    // Test 1: Single trace ingest
    let c = client.clone();
    runner.run("single trace ingest", || async move {
        let trace_id = format!("sdk-test-single-{}-{}", LANG, uid());
        let resp = post(&c, "/v1/traces", &serde_json::json!({
            "id": trace_id,
            "name": "chat-completion",
            "session_id": "test-session",
            "user_id": "test-user",
            "status": "completed",
            "input": "What is 2+2?",
            "output": "4",
            "spans": [{
                "id": format!("span-{}", uid()),
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
                "output": "4"
            }]
        })).await;
        assert_eq_test!(resp["status"].as_str().unwrap(), "accepted", "ingest status");
        wait_flush().await;

        let data = get(&c, &format!("/v1/llm/traces/{}", trace_id)).await;
        assert_eq_test!(data["trace"]["name"].as_str().unwrap(), "chat-completion", "trace name");
        assert_eq_test!(data["trace"]["total_tokens"].as_i64().unwrap(), 150, "total_tokens");
        assert_eq_test!(data["trace"]["cost_micros"].as_i64().unwrap(), 2500, "cost_micros");
        assert_eq_test!(data["spans"].as_array().unwrap().len(), 1, "span count");
        assert_eq_test!(data["spans"][0]["model"].as_str().unwrap(), "gpt-4o", "span model");
        Ok(())
    }).await;

    // Test 2: Batch trace ingest
    let c = client.clone();
    runner.run("batch trace ingest", || async move {
        let ids: Vec<String> = (0..3).map(|i| format!("sdk-test-batch-{}-{}-{}", LANG, i, uid())).collect();
        let resp = post(&c, "/v1/traces/batch", &serde_json::json!({
            "traces": [
                { "id": ids[0], "name": "gen", "spans": [{ "span_type": "generation", "model": "gpt-4o", "input_tokens": 50, "output_tokens": 25, "cost": 0.001 }] },
                { "id": ids[1], "name": "tool", "spans": [{ "span_type": "tool", "name": "search", "latency_ms": 200, "status": "ok" }] },
                { "id": ids[2], "name": "ret", "spans": [{ "span_type": "retrieval", "name": "vector", "latency_ms": 50, "status": "ok" }] },
            ]
        })).await;
        assert_eq_test!(resp["accepted"].as_i64().unwrap(), 3, "batch accepted");
        assert_eq_test!(resp["dropped"].as_i64().unwrap(), 0, "batch dropped");
        Ok(())
    }).await;

    // Test 3: Trace update
    let c = client.clone();
    runner.run("trace update", || async move {
        let trace_id = format!("sdk-test-update-{}-{}", LANG, uid());
        post(&c, "/v1/traces", &serde_json::json!({
            "id": trace_id,
            "name": "running-trace",
            "status": "running",
            "spans": [{ "span_type": "generation", "model": "gpt-4o", "status": "ok" }]
        })).await;
        wait_flush().await;

        put(&c, &format!("/v1/traces/{}", trace_id), &serde_json::json!({
            "status": "completed",
            "output": "Final answer"
        })).await;
        wait_flush().await;

        let data = get(&c, &format!("/v1/llm/traces/{}", trace_id)).await;
        assert_eq_test!(data["trace"]["status"].as_str().unwrap(), "completed", "updated status");
        Ok(())
    }).await;

    // Test 4: Token & cost accuracy
    let c = client.clone();
    runner.run("token & cost accuracy", || async move {
        let trace_id = format!("sdk-test-cost-{}-{}", LANG, uid());
        post(&c, "/v1/traces", &serde_json::json!({
            "id": trace_id,
            "name": "cost-test",
            "status": "completed",
            "spans": [{
                "span_type": "generation",
                "model": "claude-3.5-sonnet",
                "provider": "anthropic",
                "input_tokens": 1000,
                "output_tokens": 500,
                "cost": 0.012,
                "latency_ms": 3000,
                "status": "ok"
            }]
        })).await;
        wait_flush().await;

        let data = get(&c, &format!("/v1/llm/traces/{}", trace_id)).await;
        assert_eq_test!(data["trace"]["total_tokens"].as_i64().unwrap(), 1500, "total_tokens");
        assert_eq_test!(data["trace"]["cost_micros"].as_i64().unwrap(), 12000, "cost_micros");
        Ok(())
    }).await;

    // Test 5: Multi-span trace
    let c = client.clone();
    runner.run("multi-span trace", || async move {
        let trace_id = format!("sdk-test-multi-{}-{}", LANG, uid());
        let parent_span = format!("parent-{}", uid());
        let child_span = format!("child-{}", uid());

        post(&c, "/v1/traces", &serde_json::json!({
            "id": trace_id,
            "name": "multi-span",
            "status": "completed",
            "spans": [
                { "id": parent_span, "span_type": "generation", "model": "gpt-4o", "input_tokens": 100, "output_tokens": 50, "cost": 0.002, "latency_ms": 1000, "status": "ok" },
                { "id": child_span, "parent_span_id": parent_span, "span_type": "tool", "name": "fn-call", "latency_ms": 200, "status": "ok" },
            ]
        })).await;
        wait_flush().await;

        let data = get(&c, &format!("/v1/llm/traces/{}", trace_id)).await;
        assert_eq_test!(data["spans"].as_array().unwrap().len(), 2, "span count");
        Ok(())
    }).await;

    // Test 6: Error span
    let c = client.clone();
    runner.run("error span", || async move {
        let trace_id = format!("sdk-test-error-{}-{}", LANG, uid());
        post(&c, "/v1/traces", &serde_json::json!({
            "id": trace_id,
            "name": "error-trace",
            "status": "error",
            "spans": [{ "span_type": "generation", "model": "gpt-4o", "status": "error", "error_message": "Rate limit exceeded", "latency_ms": 500 }]
        })).await;
        wait_flush().await;

        let data = get(&c, &format!("/v1/llm/traces/{}", trace_id)).await;
        assert_eq_test!(data["trace"]["status"].as_str().unwrap(), "error", "trace status");
        assert_eq_test!(data["spans"][0]["status"].as_str().unwrap(), "error", "span status");
        Ok(())
    }).await;

    // Test 7: HMAC auth rejection
    let c = client.clone();
    runner.run("HMAC auth rejection", || async move {
        let status = post_raw_status(
            &c,
            "/v1/traces",
            &serde_json::json!({ "id": "should-fail", "name": "bad-auth", "spans": [{ "span_type": "generation" }] }),
            "0000000000000000000000000000000000000000000000000000000000000000",
        ).await;
        assert_eq_test!(status, 401, "auth rejection status");
        Ok(())
    }).await;

    // Test 8: Error tracking still works
    let c = client.clone();
    runner.run("error tracking still works", || async move {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let resp = post(&c, "/v1/ingest", &serde_json::json!({
            "timestamp": now,
            "source": "api",
            "environment": "test",
            "release": "0.0.1",
            "error_type": "TestError",
            "message": format!("SDK harness test {} {}", LANG, uid())
        })).await;
        let status = resp.get("status").and_then(|s| s.as_str()).unwrap_or("");
        if status != "accepted" && !resp.to_string().contains("queued") {
            return Err(format!("Unexpected response: {}", resp));
        }
        Ok(())
    }).await;

    // Summary
    println!();
    println!("# {} passed, {} failed", runner.passed, runner.failed);
    std::process::exit(if runner.failed > 0 { 1 } else { 0 });
}
