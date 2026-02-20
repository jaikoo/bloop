// ═══════════════════════════════════════════════════════════════════════════════
// LLM Proxy Tests
// ═══════════════════════════════════════════════════════════════════════════════

use bloop::llm_tracing::proxy::{
    build_trace, extract_prompt_text, OpenAIMessage, ProxyConfig, ProxyUsage,
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
        2500, // cost_micros
        1200, // latency_ms
        true, // success
        Some("stop".to_string()),
    );

    assert_eq!(trace.id, "trace-123");
    assert_eq!(trace.input, Some("Hello".to_string()));
    assert_eq!(trace.output, Some("World".to_string()));
    assert_eq!(trace.spans.len(), 1);

    let span = &trace.spans[0];
    assert_eq!(span.id, "span-456");
    assert_eq!(span.model, Some("gpt-4".to_string()));
    assert_eq!(span.provider, Some("openai".to_string()));
    assert_eq!(span.input_tokens, 10);
    assert_eq!(span.output_tokens, 20);
    assert_eq!(span.cost_micros, 2500);
    assert_eq!(span.latency_ms, 1200);
    assert_eq!(span.status, "ok");
}

#[test]
fn test_build_trace_error() {
    let trace = build_trace(
        "trace-error",
        "span-error",
        "gpt-4",
        "openai",
        Some("Input".to_string()),
        None,
        None,
        0,     // cost_micros
        5000,  // latency_ms
        false, // success
        None,
    );

    assert_eq!(trace.id, "trace-error");
    assert_eq!(trace.status, "error");
    assert_eq!(trace.spans.len(), 1);

    let span = &trace.spans[0];
    assert_eq!(span.status, "error");
    assert_eq!(span.cost_micros, 0);
    assert_eq!(span.latency_ms, 5000);
}

#[test]
fn test_build_trace_no_usage() {
    let trace = build_trace(
        "trace-no-usage",
        "span-no-usage",
        "claude-3",
        "anthropic",
        Some("Prompt".to_string()),
        Some("Response".to_string()),
        None, // no usage
        1500,
        800,
        true,
        Some("stop".to_string()),
    );

    let span = &trace.spans[0];
    assert_eq!(span.input_tokens, 0); // Defaults to 0 when no usage
    assert_eq!(span.output_tokens, 0);
    assert_eq!(span.cost_micros, 1500);
}

#[test]
fn test_build_trace_large_tokens() {
    let usage = ProxyUsage {
        prompt_tokens: 100000,
        completion_tokens: 50000,
        total_tokens: 150000,
    };

    let trace = build_trace(
        "trace-large",
        "span-large",
        "gpt-4-32k",
        "openai",
        Some("Very long prompt...".to_string()),
        Some("Very long response...".to_string()),
        Some(usage),
        5000000, // cost_micros = $5.00
        30000,   // latency_ms = 30 seconds
        true,
        Some("length".to_string()),
    );

    let span = &trace.spans[0];
    assert_eq!(span.input_tokens, 100000);
    assert_eq!(span.output_tokens, 50000);
    assert_eq!(span.cost_micros, 5000000);
    assert_eq!(span.latency_ms, 30000);
}
