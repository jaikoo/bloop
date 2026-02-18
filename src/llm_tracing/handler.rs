use crate::error::{AppError, AppResult, LoggedJson};
use crate::ingest::auth::ProjectAuth;
use crate::llm_tracing::types::*;
use crate::llm_tracing::LlmIngestState;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use std::sync::Arc;

/// POST /v1/traces - Submit a single LLM trace with nested spans.
pub async fn ingest_trace(
    State(state): State<Arc<LlmIngestState>>,
    project_auth: Option<Extension<ProjectAuth>>,
    LoggedJson(trace): LoggedJson<IngestTrace>,
) -> AppResult<Json<serde_json::Value>> {
    let project_id = project_auth
        .map(|Extension(a)| a.project_id)
        .unwrap_or_else(|| "default".to_string());

    let processed = process_trace(trace, &project_id, &state)?;
    let trace_id = processed.id.clone();
    let span_count = processed.spans.len();

    if state.tx.try_send(processed).is_err() {
        tracing::warn!(project_id = %project_id, trace_id = %trace_id, "llm tracing channel full, trace dropped");
    } else {
        tracing::info!(project_id = %project_id, trace_id = %trace_id, spans = span_count, "llm trace accepted");
    }

    Ok(Json(serde_json::json!({ "status": "accepted" })))
}

/// POST /v1/traces/batch - Submit up to 50 traces in a batch.
pub async fn ingest_trace_batch(
    State(state): State<Arc<LlmIngestState>>,
    project_auth: Option<Extension<ProjectAuth>>,
    LoggedJson(payload): LoggedJson<BatchTracePayload>,
) -> AppResult<Json<serde_json::Value>> {
    let project_id = project_auth
        .map(|Extension(a)| a.project_id)
        .unwrap_or_else(|| "default".to_string());

    if payload.traces.len() > state.config.max_batch_size {
        return Err(AppError::Validation(format!(
            "batch exceeds max size of {}",
            state.config.max_batch_size
        )));
    }

    let mut accepted = 0u64;
    let mut dropped = 0u64;
    let mut errors = Vec::new();

    for (i, trace) in payload.traces.into_iter().enumerate() {
        match process_trace(trace, &project_id, &state) {
            Ok(processed) => {
                if state.tx.try_send(processed).is_err() {
                    dropped += 1;
                } else {
                    accepted += 1;
                }
            }
            Err(e) => {
                errors.push(serde_json::json!({
                    "index": i,
                    "error": e.to_string(),
                }));
            }
        }
    }

    tracing::info!(
        project_id = %project_id,
        accepted = accepted,
        dropped = dropped,
        errors = errors.len() as u64,
        "llm trace batch processed"
    );

    Ok(Json(serde_json::json!({
        "accepted": accepted,
        "dropped": dropped,
        "errors": errors,
    })))
}

/// PUT /v1/traces/{trace_id} - Update a running trace.
pub async fn update_trace(
    State(state): State<Arc<LlmIngestState>>,
    project_auth: Option<Extension<ProjectAuth>>,
    Path(trace_id): Path<String>,
    LoggedJson(update): LoggedJson<UpdateTrace>,
) -> AppResult<Json<serde_json::Value>> {
    let project_id = project_auth
        .map(|Extension(a)| a.project_id)
        .unwrap_or_else(|| "default".to_string());

    let pool = state.pool.clone();
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let pid = project_id.clone();
    let tid = trace_id.clone();
    conn.interact(move |conn| {
        let mut updates = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut idx = 1;

        if let Some(ref status) = update.status {
            updates.push(format!("status = ?{idx}"));
            params.push(Box::new(status.clone()));
            idx += 1;
        }
        if let Some(ref output) = update.output {
            updates.push(format!("output = ?{idx}"));
            params.push(Box::new(output.clone()));
            idx += 1;
        }
        if let Some(ended_at) = update.ended_at {
            updates.push(format!("ended_at = ?{idx}"));
            params.push(Box::new(ended_at));
            idx += 1;
        }
        if let Some(input_tokens) = update.input_tokens {
            updates.push(format!("input_tokens = ?{idx}"));
            params.push(Box::new(input_tokens));
            idx += 1;
            // Also update total
            if let Some(output_tokens) = update.output_tokens {
                updates.push(format!("output_tokens = ?{idx}"));
                params.push(Box::new(output_tokens));
                idx += 1;
                updates.push(format!("total_tokens = ?{idx}"));
                params.push(Box::new(input_tokens + output_tokens));
                idx += 1;
            }
        }
        if let Some(cost) = update.cost {
            updates.push(format!("cost_micros = ?{idx}"));
            params.push(Box::new(dollars_to_micros(cost)));
            idx += 1;
        }

        if updates.is_empty() {
            return Ok(());
        }

        let sql = format!(
            "UPDATE llm_traces SET {} WHERE project_id = ?{} AND id = ?{}",
            updates.join(", "),
            idx,
            idx + 1
        );
        params.push(Box::new(pid));
        params.push(Box::new(tid));

        let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        conn.execute(&sql, refs.as_slice())?;
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::json!({ "status": "updated" })))
}

/// Validate and process an ingest trace into a ProcessedTrace.
fn process_trace(
    trace: IngestTrace,
    project_id: &str,
    state: &LlmIngestState,
) -> AppResult<ProcessedTrace> {
    // Validate trace_id length
    if let Some(ref id) = trace.id {
        if id.len() > 128 {
            return Err(AppError::Validation(
                "trace_id exceeds 128 characters".to_string(),
            ));
        }
    }

    // Validate span count
    if trace.spans.len() > state.config.max_spans_per_trace {
        return Err(AppError::Validation(format!(
            "trace has {} spans, max is {}",
            trace.spans.len(),
            state.config.max_spans_per_trace
        )));
    }

    let now = chrono::Utc::now().timestamp_millis();
    let trace_id = trace.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let started_at = trace.started_at.unwrap_or(now);

    // Get content storage policy for project
    let content_policy = state
        .settings_cache
        .get(project_id)
        .unwrap_or_else(|| ContentStorage::from_str_loose(&state.config.default_content_storage));

    // Process spans
    let mut total_input_tokens: i64 = 0;
    let mut total_output_tokens: i64 = 0;
    let mut total_cost_micros: i64 = 0;
    let mut processed_spans = Vec::with_capacity(trace.spans.len());

    for span in trace.spans {
        let span_id = span.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let span_started = span.started_at.unwrap_or(started_at);
        let span_tokens = span.input_tokens + span.output_tokens;

        // Server-side pricing: if SDK sent cost=0 but we have model + tokens, auto-calculate
        let span_cost_micros = if span.cost == 0.0
            && span.model.is_some()
            && (span.input_tokens > 0 || span.output_tokens > 0)
        {
            if let Some(ref pricing) = state.pricing {
                pricing.calculate_cost_micros(
                    span.model.as_deref().unwrap(),
                    span.input_tokens,
                    span.output_tokens,
                )
            } else {
                0
            }
        } else {
            dollars_to_micros(span.cost)
        };

        total_input_tokens += span.input_tokens;
        total_output_tokens += span.output_tokens;
        total_cost_micros += span_cost_micros;

        let (span_input, span_output, span_metadata) = strip_content(
            &content_policy,
            span.input,
            span.output,
            span.metadata.map(|v| v.to_string()),
        );

        processed_spans.push(ProcessedSpan {
            id: span_id,
            trace_id: trace_id.clone(),
            parent_span_id: span.parent_span_id,
            span_type: span.span_type,
            name: span.name,
            model: span.model,
            provider: span.provider,
            input_tokens: span.input_tokens,
            output_tokens: span.output_tokens,
            total_tokens: span_tokens,
            cost_micros: span_cost_micros,
            latency_ms: span.latency_ms,
            time_to_first_token_ms: span.time_to_first_token_ms,
            status: span.status,
            error_message: span.error_message,
            input: span_input,
            output: span_output,
            metadata: span_metadata,
            started_at: span_started,
            ended_at: span.ended_at,
        });
    }

    let (trace_input, trace_output, trace_metadata) = strip_content(
        &content_policy,
        trace.input,
        trace.output,
        trace.metadata.map(|v| v.to_string()),
    );

    Ok(ProcessedTrace {
        project_id: project_id.to_string(),
        id: trace_id,
        session_id: trace.session_id,
        user_id: trace.user_id,
        name: trace.name,
        status: trace.status,
        input_tokens: total_input_tokens,
        output_tokens: total_output_tokens,
        total_tokens: total_input_tokens + total_output_tokens,
        cost_micros: total_cost_micros,
        input: trace_input,
        output: trace_output,
        metadata: trace_metadata,
        started_at,
        ended_at: trace.ended_at,
        created_at: now,
        prompt_name: trace.prompt_name,
        prompt_version: trace.prompt_version,
        spans: processed_spans,
    })
}

/// Strip content based on project content storage policy.
fn strip_content(
    policy: &ContentStorage,
    input: Option<String>,
    output: Option<String>,
    metadata: Option<String>,
) -> (Option<String>, Option<String>, Option<String>) {
    match policy {
        ContentStorage::Full => (input, output, metadata),
        ContentStorage::MetadataOnly => (None, None, metadata),
        ContentStorage::None => (None, None, None),
    }
}
