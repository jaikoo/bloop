use crate::error::{AppError, AppResult};
use crate::llm_tracing::types::{ProcessedSpan, ProcessedTrace};
use crate::llm_tracing::LlmIngestState;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Deserialize)]
pub struct ProxyConfig {
    #[serde(default = "default_proxy_enabled")]
    pub enabled: bool,
    #[serde(default = "default_proxy_providers")]
    pub providers: Vec<String>,
    #[serde(default = "default_openai_base")]
    pub openai_base_url: String,
    #[serde(default = "default_anthropic_base")]
    pub anthropic_base_url: String,
    #[serde(default = "default_true")]
    pub capture_prompts: bool,
    #[serde(default = "default_true")]
    pub capture_completions: bool,
    #[serde(default = "default_true")]
    pub capture_streaming: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            providers: vec!["openai".to_string(), "anthropic".to_string()],
            openai_base_url: default_openai_base(),
            anthropic_base_url: default_anthropic_base(),
            capture_prompts: true,
            capture_completions: true,
            capture_streaming: true,
        }
    }
}

fn default_proxy_enabled() -> bool {
    true
}

fn default_proxy_providers() -> Vec<String> {
    vec!["openai".to_string(), "anthropic".to_string()]
}

fn default_openai_base() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_anthropic_base() -> String {
    "https://api.anthropic.com/v1".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Clone)]
pub struct ProxyState {
    pub config: ProxyConfig,
    pub client: reqwest::Client,
    pub ingest_tx: mpsc::Sender<ProcessedTrace>,
    pub pool: deadpool_sqlite::Pool,
    pub settings_cache: Arc<crate::llm_tracing::settings::SettingsCache>,
    pub pricing: Option<Arc<crate::llm_tracing::pricing::PricingTable>>,
}

impl ProxyState {
    pub fn from_ingest_state(state: &LlmIngestState, proxy_config: ProxyConfig) -> Self {
        Self {
            config: proxy_config,
            client: reqwest::Client::new(),
            ingest_tx: state.tx.clone(),
            pool: state.pool.clone(),
            settings_cache: state.settings_cache.clone(),
            pricing: state.pricing.clone(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProxyUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    #[serde(default)]
    stream: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAIMessage {
    pub role: String,
    pub content: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    id: String,
    model: String,
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    index: i64,
    message: Option<OpenAIMessage>,
    #[serde(default)]
    delta: Option<OpenAIDelta>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAIDelta {
    role: Option<String>,
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChunk {
    id: String,
    choices: Vec<OpenAIStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    delta: OpenAIDelta,
    finish_reason: Option<String>,
}

pub async fn proxy_handler(
    Path((provider, path)): Path<(String, String)>,
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    if !state.config.enabled {
        return Err(AppError::Validation("Proxy is disabled".to_string()));
    }

    match provider.as_str() {
        "openai" => proxy_openai(state, headers, body, path).await,
        _ => Err(AppError::Validation(format!(
            "Unknown provider: {}",
            provider
        ))),
    }
}

async fn proxy_openai(
    state: Arc<ProxyState>,
    headers: HeaderMap,
    body: Bytes,
    path: String,
) -> AppResult<Response> {
    let start_time = Instant::now();
    let trace_id = uuid::Uuid::new_v4().to_string();
    let span_id = uuid::Uuid::new_v4().to_string();

    let request: OpenAIRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(_) => {
            return forward_request(&state, &state.config.openai_base_url, &path, headers, body)
                .await;
        }
    };

    let model = request.model.clone();
    let is_streaming = request.stream;
    let prompt_text = if state.config.capture_prompts {
        extract_prompt_text(&request.messages)
    } else {
        None
    };

    let upstream_url = format!("{}/{}", state.config.openai_base_url, path);
    let ingest_tx = state.ingest_tx.clone();
    let pricing = state.pricing.clone();
    let capture_completions = state.config.capture_completions;

    if is_streaming {
        handle_streaming(
            &state,
            &upstream_url,
            headers,
            body,
            trace_id,
            span_id,
            model,
            prompt_text,
            start_time,
            ingest_tx,
            pricing,
            capture_completions,
        )
        .await
    } else {
        handle_non_streaming(
            &state,
            &upstream_url,
            headers,
            body,
            trace_id,
            span_id,
            model,
            prompt_text,
            start_time,
            ingest_tx,
            pricing,
            capture_completions,
        )
        .await
    }
}

pub fn extract_prompt_text(messages: &[OpenAIMessage]) -> Option<String> {
    let texts: Vec<String> = messages
        .iter()
        .filter_map(|m| match &m.content {
            serde_json::Value::String(s) => Some(format!("[{}]: {}", m.role, s)),
            serde_json::Value::Array(arr) => {
                let parts: Vec<String> = arr
                    .iter()
                    .filter_map(|item| {
                        item.get("text")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                if parts.is_empty() {
                    None
                } else {
                    Some(format!("[{}]: {}", m.role, parts.join(" ")))
                }
            }
            _ => None,
        })
        .collect();

    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

async fn forward_request(
    state: &ProxyState,
    base_url: &str,
    path: &str,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let url = format!("{}/{}", base_url, path);
    let mut req = state.client.request(reqwest::Method::POST, &url);

    for (key, value) in headers.iter() {
        if key.as_str().eq_ignore_ascii_case("authorization")
            || key.as_str().eq_ignore_ascii_case("content-type")
        {
            req = req.header(key, value);
        }
    }

    let response = req
        .body(body)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Upstream error: {}", e)))?;

    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::OK);

    let body_bytes = response
        .bytes()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read response: {}", e)))?;

    let response_builder = Response::builder().status(status);

    Ok(response_builder.body(Body::from(body_bytes)).unwrap())
}

async fn handle_non_streaming(
    state: &ProxyState,
    upstream_url: &str,
    headers: HeaderMap,
    body: Bytes,
    trace_id: String,
    span_id: String,
    model: String,
    prompt_text: Option<String>,
    start_time: Instant,
    ingest_tx: mpsc::Sender<ProcessedTrace>,
    pricing: Option<Arc<crate::llm_tracing::pricing::PricingTable>>,
    capture_completions: bool,
) -> AppResult<Response> {
    let mut req = state.client.post(upstream_url);

    for (key, value) in headers.iter() {
        if key.as_str().eq_ignore_ascii_case("authorization")
            || key.as_str().eq_ignore_ascii_case("content-type")
        {
            req = req.header(key, value);
        }
    }

    let upstream_response = req
        .body(body)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Upstream error: {}", e)))?;

    let status = upstream_response.status();
    let headers_clone = upstream_response.headers().clone();

    let response_body = upstream_response
        .bytes()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read response: {}", e)))?;

    let (completion_text, usage) = if status.is_success() {
        match serde_json::from_slice::<OpenAIResponse>(&response_body) {
            Ok(parsed) => {
                let text = if capture_completions {
                    parsed
                        .choices
                        .first()
                        .and_then(|c| c.message.as_ref())
                        .map(|m| match &m.content {
                            serde_json::Value::String(s) => s.clone(),
                            _ => m.content.to_string(),
                        })
                } else {
                    None
                };
                (text, parsed.usage)
            }
            Err(_) => (None, None),
        }
    } else {
        (None, None)
    };

    let latency_ms = start_time.elapsed().as_millis() as i64;
    let cost_micros = if let (Some(ref p), Some(ref u)) = (&pricing, &usage) {
        p.calculate_cost_micros(&model, u.prompt_tokens, u.completion_tokens)
    } else {
        0
    };

    let trace = build_trace(
        &trace_id,
        &span_id,
        &model,
        "openai",
        prompt_text,
        completion_text,
        usage.map(|u| ProxyUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        }),
        cost_micros,
        latency_ms,
        status.is_success(),
        None,
    );

    if let Err(e) = ingest_tx.try_send(trace) {
        tracing::warn!(error = %e, trace_id = %trace_id, "Failed to queue proxy trace");
    }

    let mut response_builder =
        Response::builder().status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK));

    for (key, value) in headers_clone.iter() {
        if let Ok(name) = axum::http::HeaderName::from_bytes(key.as_str().as_bytes()) {
            response_builder = response_builder.header(name, value);
        }
    }

    Ok(response_builder.body(Body::from(response_body)).unwrap())
}

async fn handle_streaming(
    state: &ProxyState,
    upstream_url: &str,
    headers: HeaderMap,
    body: Bytes,
    trace_id: String,
    span_id: String,
    model: String,
    prompt_text: Option<String>,
    start_time: Instant,
    ingest_tx: mpsc::Sender<ProcessedTrace>,
    pricing: Option<Arc<crate::llm_tracing::pricing::PricingTable>>,
    capture_completions: bool,
) -> AppResult<Response> {
    let mut req = state.client.post(upstream_url);

    for (key, value) in headers.iter() {
        if key.as_str().eq_ignore_ascii_case("authorization")
            || key.as_str().eq_ignore_ascii_case("content-type")
        {
            req = req.header(key, value);
        }
    }

    let upstream_response = req
        .body(body)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Upstream error: {}", e)))?;

    let status = upstream_response.status();

    if !status.is_success() {
        let body = upstream_response
            .bytes()
            .await
            .map_err(|e| AppError::Internal(format!("Failed to read error: {}", e)))?;
        return Ok(Response::builder()
            .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK))
            .body(Body::from(body))
            .unwrap());
    }

    let model_clone = model.clone();
    let trace_id_clone = trace_id.clone();
    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Bytes>(100);

    let _capture_handle = tokio::spawn(async move {
        let mut completion_parts = Vec::new();
        let mut finish_reason = None;

        while let Some(chunk) = chunk_rx.recv().await {
            if capture_completions {
                if let Ok(text) = String::from_utf8(chunk.to_vec()) {
                    for line in text.lines() {
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                finish_reason = Some("stop".to_string());
                                break;
                            }
                            if let Ok(chunk) = serde_json::from_str::<OpenAIStreamChunk>(data) {
                                for choice in chunk.choices {
                                    if let Some(content) = choice.delta.content {
                                        completion_parts.push(content);
                                    }
                                    if choice.finish_reason.is_some() {
                                        finish_reason = choice.finish_reason.clone();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let completion_text = if capture_completions && !completion_parts.is_empty() {
            Some(completion_parts.join(""))
        } else {
            None
        };

        let latency_ms = start_time.elapsed().as_millis() as i64;
        let estimated_completion_tokens = completion_parts.len() as i64 * 4;

        let usage = ProxyUsage {
            prompt_tokens: 0,
            completion_tokens: estimated_completion_tokens,
            total_tokens: estimated_completion_tokens,
        };

        let cost_micros = if let Some(ref p) = pricing {
            p.calculate_cost_micros(&model_clone, 0, estimated_completion_tokens)
        } else {
            0
        };

        let trace = build_trace(
            &trace_id_clone,
            &span_id,
            &model_clone,
            "openai",
            prompt_text,
            completion_text,
            Some(usage),
            cost_micros,
            latency_ms,
            true,
            finish_reason,
        );

        if let Err(e) = ingest_tx.try_send(trace) {
            tracing::warn!(error = %e, trace_id = %trace_id_clone, "Failed to queue streaming proxy trace");
        }
    });

    let stream =
        upstream_response
            .bytes_stream()
            .map(move |result: Result<Bytes, reqwest::Error>| match result {
                Ok(bytes) => {
                    let _ = chunk_tx.try_send(bytes.clone());
                    Ok::<_, std::convert::Infallible>(bytes)
                }
                Err(_) => Ok::<_, std::convert::Infallible>(Bytes::new()),
            });

    let body = Body::from_stream(stream);

    let response = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(body)
        .unwrap();

    Ok(response)
}

pub fn build_trace(
    trace_id: &str,
    span_id: &str,
    model: &str,
    provider: &str,
    input: Option<String>,
    output: Option<String>,
    usage: Option<ProxyUsage>,
    cost_micros: i64,
    latency_ms: i64,
    success: bool,
    finish_reason: Option<String>,
) -> ProcessedTrace {
    let now = chrono::Utc::now().timestamp_millis();

    let span = ProcessedSpan {
        id: span_id.to_string(),
        trace_id: trace_id.to_string(),
        parent_span_id: None,
        span_type: "generation".to_string(),
        name: format!("{} {}", provider, model),
        model: Some(model.to_string()),
        provider: Some(provider.to_string()),
        input_tokens: usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
        output_tokens: usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
        total_tokens: usage.as_ref().map(|u| u.total_tokens).unwrap_or(0),
        cost_micros,
        latency_ms,
        time_to_first_token_ms: None,
        status: if success {
            "ok".to_string()
        } else {
            "error".to_string()
        },
        error_message: if success {
            None
        } else {
            Some("Proxy error".to_string())
        },
        input,
        output,
        metadata: finish_reason.map(|r| json!({"finish_reason": r}).to_string()),
        started_at: now - latency_ms,
        ended_at: Some(now),
    };

    ProcessedTrace {
        project_id: "default".to_string(),
        id: trace_id.to_string(),
        session_id: None,
        user_id: None,
        name: format!("{} completion", provider),
        status: if success {
            "completed".to_string()
        } else {
            "failed".to_string()
        },
        input_tokens: span.input_tokens,
        output_tokens: span.output_tokens,
        total_tokens: span.total_tokens,
        cost_micros,
        input: span.input.clone(),
        output: span.output.clone(),
        metadata: None,
        started_at: span.started_at,
        ended_at: span.ended_at,
        created_at: now,
        prompt_name: None,
        prompt_version: None,
        spans: vec![span],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_prompt_text() {
        let messages = vec![
            OpenAIMessage {
                role: "system".to_string(),
                content: serde_json::Value::String("You are helpful".to_string()),
            },
            OpenAIMessage {
                role: "user".to_string(),
                content: serde_json::Value::String("Hello".to_string()),
            },
        ];

        let result = extract_prompt_text(&messages);
        assert!(result.is_some());
        assert!(result.unwrap().contains("system"));
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
        assert!(result.unwrap().contains("What's in this image"));
    }

    #[test]
    fn test_build_trace() {
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
            2500,
            1200,
            true,
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
        assert_eq!(trace.spans[0].model, Some("gpt-4".to_string()));
        assert_eq!(trace.spans[0].provider, Some("openai".to_string()));
    }

    #[test]
    fn test_proxy_config_default() {
        let config = ProxyConfig::default();
        assert!(config.enabled);
        assert!(config.capture_prompts);
        assert!(config.capture_completions);
        assert!(config.capture_streaming);
        assert_eq!(config.openai_base_url, "https://api.openai.com/v1");
        assert_eq!(config.anthropic_base_url, "https://api.anthropic.com/v1");
    }
}
