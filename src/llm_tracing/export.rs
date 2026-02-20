use crate::auth::bearer::TokenAuth;
use crate::error::{AppError, AppResult};
use crate::llm_tracing::LlmQueryState;
use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct ExportQueryParams {
    pub project_id: Option<String>,
    pub hours: Option<i64>,
    pub trace_ids: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LangChainRun {
    pub id: String,
    pub name: String,
    pub run_type: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub extra: HashMap<String, serde_json::Value>,
    pub error: Option<String>,
    pub inputs: HashMap<String, serde_json::Value>,
    pub outputs: HashMap<String, serde_json::Value>,
    pub events: Vec<LangChainEvent>,
    pub parent_run_id: Option<String>,
    pub child_run_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct LangChainEvent {
    pub name: String,
    pub time: String,
    pub kwargs: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct LangChainExportResponse {
    pub runs: Vec<LangChainRun>,
    pub count: usize,
}

pub async fn export_langsmith(
    State(state): State<Arc<LlmQueryState>>,
    _token_auth: Option<axum::Extension<TokenAuth>>,
    Query(params): Query<ExportQueryParams>,
) -> AppResult<Json<LangChainExportResponse>> {
    let project_id = params.project_id.clone();
    let hours = params.hours.unwrap_or(24).clamp(1, 720);

    let trace_ids: Option<Vec<String>> = params
        .trace_ids
        .as_ref()
        .map(|s| s.split(',').map(|id| id.trim().to_string()).collect());

    let pool = state.pool.clone();
    let traces = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("Pool error: {}", e)))?
        .interact(move |conn| {
            let mut stmt = if let Some(ref _ids) = trace_ids {
                conn.prepare(
                    "SELECT id, name, status, input, output, started_at, ended_at, input_tokens, output_tokens, total_tokens, cost_micros, metadata
                     FROM llm_traces
                     WHERE project_id = ?1 AND id IN (SELECT value FROM json_each(?2))
                     ORDER BY started_at DESC"
                )?
            } else {
                conn.prepare(
                    "SELECT id, name, status, input, output, started_at, ended_at, input_tokens, output_tokens, total_tokens, cost_micros, metadata
                     FROM llm_traces
                     WHERE project_id = ?1 AND started_at >= ?2
                     ORDER BY started_at DESC
                     LIMIT 100"
                )?
            };

            let project_id_ref = project_id.as_deref().unwrap_or("default");
            let mut rows = if let Some(ref ids) = trace_ids {
                let ids_json = serde_json::to_string(ids).unwrap_or_default();
                stmt.query(rusqlite::params![project_id_ref, ids_json])?
            } else {
                let since = chrono::Utc::now().timestamp_millis() - (hours * 3_600_000);
                stmt.query(rusqlite::params![project_id_ref, since])?
            };

            let mut traces = Vec::new();
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let status: String = row.get(2)?;
                let input: Option<String> = row.get(3)?;
                let output: Option<String> = row.get(4)?;
                let started_at: i64 = row.get(5)?;
                let ended_at: Option<i64> = row.get(6)?;
                let input_tokens: i64 = row.get(7)?;
                let output_tokens: i64 = row.get(8)?;
                let total_tokens: i64 = row.get(9)?;
                let cost_micros: i64 = row.get(10)?;
                let metadata: Option<String> = row.get(11)?;

                traces.push((
                    id, name, status, input, output, started_at, ended_at,
                    input_tokens, output_tokens, total_tokens, cost_micros, metadata
                ));
            }

            Ok::<_, rusqlite::Error>(traces)
        })
        .await
        .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?
        .map_err(AppError::Database)?;

    let runs: Vec<LangChainRun> = traces
        .into_iter()
        .map(
            |(
                id,
                name,
                status,
                input,
                output,
                started_at,
                ended_at,
                input_tokens,
                output_tokens,
                total_tokens,
                cost_micros,
                metadata,
            )| {
                let mut extra = HashMap::new();
                extra.insert("status".to_string(), serde_json::json!(status));
                extra.insert("input_tokens".to_string(), serde_json::json!(input_tokens));
                extra.insert(
                    "output_tokens".to_string(),
                    serde_json::json!(output_tokens),
                );
                extra.insert("total_tokens".to_string(), serde_json::json!(total_tokens));
                extra.insert("cost_micros".to_string(), serde_json::json!(cost_micros));

                if let Some(meta) = metadata {
                    if let Ok(meta_json) = serde_json::from_str::<serde_json::Value>(&meta) {
                        extra.insert("metadata".to_string(), meta_json);
                    }
                }

                let mut inputs = HashMap::new();
                if let Some(inp) = input {
                    inputs.insert("input".to_string(), serde_json::json!(inp));
                }

                let mut outputs = HashMap::new();
                if let Some(out) = output {
                    outputs.insert("output".to_string(), serde_json::json!(out));
                }

                let start_time = chrono::DateTime::from_timestamp_millis(started_at)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default();

                let end_time = ended_at.and_then(|ts| {
                    chrono::DateTime::from_timestamp_millis(ts).map(|dt| dt.to_rfc3339())
                });

                LangChainRun {
                    id: id.clone(),
                    name,
                    run_type: "llm".to_string(),
                    start_time,
                    end_time,
                    extra,
                    error: if status == "error" {
                        Some("LLM call failed".to_string())
                    } else {
                        None
                    },
                    inputs,
                    outputs,
                    events: vec![],
                    parent_run_id: None,
                    child_run_ids: vec![],
                }
            },
        )
        .collect();

    let count = runs.len();

    Ok(Json(LangChainExportResponse { runs, count }))
}

#[derive(Debug, Deserialize)]
pub struct OpenTelemetryExportRequest {
    pub resource_spans: Vec<ResourceSpan>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceSpan {
    pub resource: Resource,
    pub scope_spans: Vec<ScopeSpan>,
}

#[derive(Debug, Deserialize)]
pub struct Resource {
    pub attributes: Vec<KeyValue>,
}

#[derive(Debug, Deserialize)]
pub struct ScopeSpan {
    pub scope: InstrumentationScope,
    pub spans: Vec<Span>,
}

#[derive(Debug, Deserialize)]
pub struct InstrumentationScope {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub start_time_unix_nano: i64,
    pub end_time_unix_nano: i64,
    pub attributes: Vec<KeyValue>,
    pub events: Vec<Event>,
}

#[derive(Debug, Deserialize)]
pub struct KeyValue {
    pub key: String,
    pub value: AttributeValue,
}

#[derive(Debug, Deserialize)]
pub struct AttributeValue {
    pub string_value: Option<String>,
    pub int_value: Option<i64>,
    pub bool_value: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct Event {
    pub name: String,
    pub time_unix_nano: i64,
    pub attributes: Vec<KeyValue>,
}

pub async fn import_otlp(
    State(_state): State<Arc<LlmQueryState>>,
    Json(payload): Json<OpenTelemetryExportRequest>,
) -> AppResult<Json<serde_json::Value>> {
    use crate::llm_tracing::types::ProcessedSpan;
    use crate::llm_tracing::types::ProcessedTrace;

    let mut traces = Vec::new();

    for resource_span in payload.resource_spans {
        for scope_span in resource_span.scope_spans {
            for span in scope_span.spans {
                let trace_id = hex::decode(&span.trace_id)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                    .unwrap_or_else(|| span.trace_id.clone());

                let span_id = hex::decode(&span.span_id)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                    .unwrap_or_else(|| span.span_id.clone());

                let attrs: HashMap<String, String> = span
                    .attributes
                    .into_iter()
                    .filter_map(|kv| {
                        let val = kv
                            .value
                            .string_value
                            .or_else(|| kv.value.int_value.map(|v| v.to_string()))
                            .or_else(|| kv.value.bool_value.map(|v| v.to_string()))
                            .unwrap_or_default();
                        Some((kv.key, val))
                    })
                    .collect();

                let model = attrs.get("llm.model").cloned();
                let provider = attrs.get("llm.provider").cloned();
                let input_tokens = attrs
                    .get("llm.input_tokens")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0i64);
                let output_tokens = attrs
                    .get("llm.output_tokens")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0i64);

                let processed_span = ProcessedSpan {
                    id: span_id.clone(),
                    trace_id: trace_id.clone(),
                    parent_span_id: span.parent_span_id.clone(),
                    span_type: attrs
                        .get("span.type")
                        .cloned()
                        .unwrap_or_else(|| "llm".to_string()),
                    name: span.name.clone(),
                    model: model.clone(),
                    provider: provider.clone(),
                    input_tokens,
                    output_tokens,
                    total_tokens: input_tokens + output_tokens,
                    cost_micros: 0,
                    latency_ms: (span.end_time_unix_nano - span.start_time_unix_nano) / 1_000_000,
                    time_to_first_token_ms: None,
                    status: "ok".to_string(),
                    error_message: None,
                    input: attrs.get("llm.prompt").cloned(),
                    output: attrs.get("llm.completion").cloned(),
                    metadata: Some(serde_json::to_string(&attrs).unwrap_or_default()),
                    started_at: span.start_time_unix_nano / 1_000_000,
                    ended_at: Some(span.end_time_unix_nano / 1_000_000),
                };

                let trace = ProcessedTrace {
                    project_id: "default".to_string(),
                    id: trace_id,
                    session_id: None,
                    user_id: None,
                    name: span.name,
                    status: "completed".to_string(),
                    input_tokens: processed_span.input_tokens,
                    output_tokens: processed_span.output_tokens,
                    total_tokens: processed_span.total_tokens,
                    cost_micros: 0,
                    input: processed_span.input.clone(),
                    output: processed_span.output.clone(),
                    metadata: processed_span.metadata.clone(),
                    started_at: processed_span.started_at,
                    ended_at: processed_span.ended_at,
                    created_at: chrono::Utc::now().timestamp_millis(),
                    prompt_name: None,
                    prompt_version: None,
                    spans: vec![processed_span],
                };

                traces.push(trace);
            }
        }
    }

    tracing::info!(count = traces.len(), "Imported OTLP traces");

    Ok(Json(serde_json::json!({
        "partial_success": serde_json::Value::Null,
    })))
}
