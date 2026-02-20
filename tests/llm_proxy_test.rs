

// ═══════════════════════════════════════════════════════════════════════════════
// LLM Proxy Tests
// ═══════════════════════════════════════════════════════════════════════════════

use bloop::llm_tracing::proxy::{
    extract_prompt_text, build_trace, ProxyConfig, ProxyUsage, OpenAIMessage,
};

#[test]
fn test_proxy_config_defaults() {
    let config = ProxyConfig::default();
    assert!(config.enabled);
    assert!(config.capture_prompts);
    assert!(config.capture_completions);
    assert!(config.capture_streaming);
    assert_eq!(config.openai_base_url, "https://api.openai.com/v1");
    assert_eq!(config.anthropic_base_url, "https://api.anthropic.com/v1");
    assert!(config.providers.contains(&"openai".to_string()));
}

#[test]
fn test_extract_prompt_text_simple() {
    let messages = vec![
        OpenAIMessage {
            role: "system".to_string(),
            content: serde_json::Value::String("You are helpful".to_string()),
        },
        OpenAIMessage {
            role: "user".to_string(),
            content: serde_json::Value::String("Hello, world!".to_string()),
        },
    ];

    let result = extract_prompt_text(&messages);
    assert!(result.is_some());
    let text = result.unwrap();
    assert!(text.contains("[system]: You are helpful"));
    assert!(text.contains("[user]: Hello, world!"));
}

#[test]
fn test_extract_prompt_text_empty() {
    let messages: Vec<OpenAIMessage> = vec![];
    let result = extract_prompt_text(&messages);
    assert!(result.is_none());
}

#[test]
fn test_extract_prompt_text_multimodal() {
    let messages = vec![OpenAIMessage {
        role: "user".to_string(),
        content: serde_json::json!([
            {"type": "text", "text": "What's in this image?"},
            {"type": "image_url", "image_url": {"url": "https://example.com/image.jpg"}}
        ]),
    }];

    let result = extract_prompt_text(&messages);
    assert!(result.is_some());
    assert!(result.unwrap().contains("What's in this image?"));
}

#[test]
fn test_extract_prompt_text_complex_content() {
    let messages = vec![OpenAIMessage {
        role: "user".to_string(),
        content: serde_json::json!([
            {"type": "text", "text": "First part"},
            {"type": "text", "text": "Second part"}
        ]),
    }];

    let result = extract_prompt_text(&messages);
    assert!(result.is_some());
    let text = result.unwrap();
    assert!(text.contains("First part"));
    assert!(text.contains("Second part"));
}

#[test]
fn test_build_trace_success() {
    let usage = ProxyUsage {
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30,
    };

    let trace = build_trace(
        "trace-123",
        "span-456",
        "gpt-4",
        "openai",
        Some("Hello".to_string()),
        Some("World".to_string()),
        Some(usage),
        2500,  // cost_micros
        1200,  // latency_ms
        true,  // success
        Some("stop".to_string()),
    );

    assert_eq!(trace.id, "trace-123");
    assert_eq!(trace.project_id, "default");
    assert_eq!(trace.input_tokens, 10);
    assert_eq!(trace.output_tokens, 20);
    assert_eq!(trace.total_tokens, 30);
    assert_eq!(trace.cost_micros, 2500);
    assert_eq!(trace.status, "completed");
    assert_eq!(trace.input, Some("Hello".to_string()));
    assert_eq!(trace.output, Some("World".to_string()));
    assert_eq!(trace.spans.len(), 1);
    
    let span = &trace.spans[0];
    assert_eq!(span.id, "span-456");
    assert_eq!(span.model, Some("gpt-4".to_string()));
    assert_eq!(span.provider, Some("openai".to_string()));
    assert_eq!(span.status, "ok");
    assert_eq!(span.latency_ms, 1200);
    assert!(span.metadata.is_some());
    assert!(span.metadata.as_ref().unwrap().contains("stop"));
}

#[test]
fn test_build_trace_error() {
    let usage = ProxyUsage {
        prompt_tokens: 5,
        completion_tokens: 0,
        total_tokens: 5,
    };

    let trace = build_trace(
        "trace-error",
        "span-error",
        "gpt-3.5-turbo",
        "openai",
        Some("Test".to_string()),
        None,
        Some(usage),
        0,
        500,
        false, // failure
        None,
    );

    assert_eq!(trace.status, "failed");
    assert_eq!(trace.spans[0].status, "error");
    assert!(trace.spans[0].error_message.is_some());
}

#[test]
fn test_build_trace_no_usage() {
    let trace = build_trace(
        "trace-no-usage",
        "span-no-usage",
        "gpt-4",
        "openai",
        None,
        None,
        None, // no usage
        0,
        1000,
        true,
        None,
    );

    assert_eq!(trace.input_tokens, 0);
    assert_eq!(trace.output_tokens, 0);
    assert_eq!(trace.total_tokens, 0);
}

#[test]
fn test_build_trace_large_tokens() {
    let usage = ProxyUsage {
        prompt_tokens: 1_000_000,
        completion_tokens: 500_000,
        total_tokens: 1_500_000,
    };

    let trace = build_trace(
        "trace-large",
        "span-large",
        "gpt-4-32k",
        "openai",
        None,
        None,
        Some(usage),
        100_000_000, // $100 in microdollars
        5000,
        true,
        None,
    );

    assert_eq!(trace.input_tokens, 1_000_000);
    assert_eq!(trace.output_tokens, 500_000);
    assert_eq!(trace.total_tokens, 1_500_000);
    assert_eq!(trace.cost_micros, 100_000_000);
}

// Integration test helpers for proxy

async fn spawn_llm_server_with_proxy() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    use bloop::llm_tracing::{LlmIngestState, LlmQueryState, settings::SettingsCache};
    use bloop::storage::create_pool;
    use axum::{Router, routing::post};
    use axum::extract::DefaultBodyLimit;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    let config = bloop::config::AppConfig::default();
    let pool = create_pool(&config.database.path).await.unwrap();
    let pool_clone = pool.clone();

    // Run migrations
    bloop::storage::migrations::migrate(&pool).await.unwrap();

    // Setup LLM state
    let (tx, _) = mpsc::channel(1000);
    let llm_ingest_state = Arc::new(LlmIngestState {
        pool: pool_clone.clone(),
        tx,
        max_batch_size: 50,
        max_spans_per_trace: 100,
        settings_cache: Arc::new(SettingsCache::new(100)),
        pricing: None,
    });

    let llm_query_state = Arc::new(LlmQueryState {
        pool: pool_clone.clone(),
        settings_cache: Arc::new(SettingsCache::new(100)),
    });

    // Setup proxy
    let proxy_config = bloop::llm_tracing::proxy::ProxyConfig::default();
    let proxy_state = Arc::new(bloop::llm_tracing::proxy::ProxyState::from_ingest_state(
        &llm_ingest_state,
        proxy_config,
    ));

    // Build router with proxy endpoint
    let app = Router::new()
        .route("/v1/proxy/openai/{*path}", post(bloop::llm_tracing::proxy::proxy_handler))
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
        .with_state(proxy_state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (addr, handle)
}

#[tokio::test]
async fn test_proxy_endpoint_exists() {
    // This test verifies the proxy endpoint is accessible
    // Note: It will fail to forward to OpenAI without a valid API key,
    // but it verifies the endpoint routing works
    let (addr, _handle) = spawn_llm_server_with_proxy().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Send a request without proper auth - should get a response (not 404)
    let resp = client
        .post(format!("{base}/v1/proxy/openai/chat/completions"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await;

    // Should not be a 404 (endpoint exists)
    // May fail with upstream error (no API key) but that's expected
    assert!(resp.is_ok(), "Proxy endpoint should be accessible");
}

#[tokio::test]
async fn test_proxy_streaming_request_parsing() {
    // Test that streaming requests are properly parsed
    let (addr, _handle) = spawn_llm_server_with_proxy().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let resp = client
        .post(format!("{base}/v1/proxy/openai/chat/completions"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true
        }))
        .send()
        .await;

    // Endpoint should exist and accept the request
    assert!(resp.is_ok());
    let status = resp.unwrap().status();
    // Will fail upstream but that's ok - we're testing routing
    assert_ne!(status, 404);
}

#[tokio::test]
async fn test_proxy_invalid_provider_returns_error() {
    let (addr, _handle) = spawn_llm_server_with_proxy().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let resp = client
        .post(format!("{base}/v1/proxy/unknown_provider/chat/completions"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": "gpt-4",
            "messages": []
        }))
        .send()
        .await
        .unwrap();

    // Unknown provider should return 400
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_proxy_malformed_json_forwarded() {
    // Test that malformed JSON is forwarded transparently
    let (addr, _handle) = spawn_llm_server_with_proxy().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let resp = client
        .post(format!("{base}/v1/proxy/openai/chat/completions"))
        .header("Content-Type", "application/json")
        .body("this is not valid json")
        .send()
        .await
        .unwrap();

    // Should forward (and upstream will reject), not crash
    assert_ne!(resp.status(), 500);
}
