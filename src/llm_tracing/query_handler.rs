use crate::auth::bearer::TokenAuth;
use crate::error::{AppError, AppResult};
use crate::llm_tracing::query;
use crate::llm_tracing::types::*;
use crate::llm_tracing::LlmQueryState;
use crate::query::handler::resolve_project_for_scope;
use axum::extract::{Path, Query, State};
use axum::Json;
use duckdb::params;
use rusqlite::params as sqlite_params;
use std::sync::Arc;

fn resolve_project(
    token_auth: &Option<axum::Extension<TokenAuth>>,
    query_project_id: Option<String>,
) -> AppResult<Option<String>> {
    resolve_project_for_scope(token_auth, query_project_id, "errors:read")
}

/// GET /v1/llm/overview
pub async fn overview(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<OverviewResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);
    let project_id = qp.project_id.clone();

    let result = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::OVERVIEW_SQL)?;
            stmt.query_row(params![since, project_id], |row| {
                let total_traces: i64 = row.get(0)?;
                let total_spans: i64 = row.get(1)?;
                let total_input_tokens: i64 = row.get(2)?;
                let total_output_tokens: i64 = row.get(3)?;
                let total_tokens: i64 = row.get(4)?;
                let total_cost_micros: i64 = row.get(5)?;
                let error_count: i64 = row.get(6)?;
                let error_rate = if total_spans > 0 {
                    error_count as f64 * 100.0 / total_spans as f64
                } else {
                    0.0
                };
                Ok(OverviewResponse {
                    total_traces,
                    total_spans,
                    total_input_tokens,
                    total_output_tokens,
                    total_tokens,
                    total_cost_micros,
                    error_count,
                    error_rate,
                    window_hours: hours,
                })
            })
        })
        .await
        .map_err(AppError::LlmTracing)?;

    Ok(Json(result))
}

/// GET /v1/llm/usage
pub async fn usage(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<UsageResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let limit = qp.limit();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);
    let project_id = qp.project_id.clone();
    let model = qp.model.clone();
    let provider = qp.provider.clone();

    let usage = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::USAGE_SQL)?;
            let rows =
                stmt.query_map(params![since, project_id, model, provider, limit], |row| {
                    Ok(UsageEntry {
                        hour_bucket: row.get(0)?,
                        model: row.get(1)?,
                        provider: row.get(2)?,
                        span_count: row.get(3)?,
                        input_tokens: row.get(4)?,
                        output_tokens: row.get(5)?,
                        total_tokens: row.get(6)?,
                        cost_micros: row.get(7)?,
                    })
                })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(AppError::LlmTracing)?;

    Ok(Json(UsageResponse {
        usage,
        window_hours: hours,
    }))
}

/// GET /v1/llm/latency
pub async fn latency(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<LatencyResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);
    let project_id = qp.project_id.clone();
    let model = qp.model.clone();

    let latency = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::LATENCY_SQL)?;
            let rows = stmt.query_map(params![since, project_id, model], |row| {
                Ok(LatencyEntry {
                    model: row.get(0)?,
                    p50_ms: row.get(1)?,
                    p90_ms: row.get(2)?,
                    p99_ms: row.get(3)?,
                    avg_ttft_ms: row.get(4)?,
                    span_count: row.get(5)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(AppError::LlmTracing)?;

    Ok(Json(LatencyResponse {
        latency,
        window_hours: hours,
    }))
}

/// GET /v1/llm/models
pub async fn models(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<ModelsResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let limit = qp.limit();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);
    let project_id = qp.project_id.clone();

    let models = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::MODELS_SQL)?;
            let rows = stmt.query_map(params![since, project_id, limit], |row| {
                Ok(ModelEntry {
                    model: row.get(0)?,
                    provider: row.get(1)?,
                    span_count: row.get(2)?,
                    input_tokens: row.get(3)?,
                    output_tokens: row.get(4)?,
                    total_tokens: row.get(5)?,
                    cost_micros: row.get(6)?,
                    error_count: row.get(7)?,
                    error_rate: row.get(8)?,
                    avg_latency_ms: row.get(9)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(AppError::LlmTracing)?;

    Ok(Json(ModelsResponse {
        models,
        window_hours: hours,
    }))
}

/// GET /v1/llm/traces
pub async fn traces_list(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<TraceListResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let limit = qp.limit();
    let offset = qp.offset();
    let sort_key = qp.sort_key().to_string();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);
    let project_id = qp.project_id.clone();
    let session_id = qp.session_id.clone();
    let status = qp.status.clone();
    let user_id = qp.user_id.clone();
    let min_cost_micros = qp.min_cost_micros;
    let prompt_name = qp.prompt_name.clone();
    let prompt_version = qp.prompt_version.clone();

    let project_id2 = project_id.clone();
    let session_id2 = session_id.clone();
    let status2 = status.clone();
    let user_id2 = user_id.clone();
    let min_cost_micros2 = min_cost_micros;
    let prompt_name2 = prompt_name.clone();
    let prompt_version2 = prompt_version.clone();

    let traces = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::TRACES_LIST_SQL)?;
            let rows = stmt.query_map(
                params![
                    since,
                    project_id,
                    session_id,
                    status,
                    user_id,
                    min_cost_micros,
                    sort_key,
                    limit,
                    offset,
                    prompt_name,
                    prompt_version
                ],
                |row| {
                    Ok(TraceListEntry {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        name: row.get(2)?,
                        status: row.get(3)?,
                        total_tokens: row.get(4)?,
                        cost_micros: row.get(5)?,
                        span_count: row.get(6)?,
                        started_at: row.get(7)?,
                        ended_at: row.get(8)?,
                        prompt_name: row.get(9)?,
                        prompt_version: row.get(10)?,
                    })
                },
            )?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(AppError::LlmTracing)?;

    let total = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::TRACES_COUNT_SQL)?;
            stmt.query_row(
                params![
                    since,
                    project_id2,
                    session_id2,
                    status2,
                    user_id2,
                    min_cost_micros2,
                    prompt_name2,
                    prompt_version2
                ],
                |row| row.get::<_, i64>(0),
            )
        })
        .await
        .map_err(AppError::LlmTracing)?;

    Ok(Json(TraceListResponse {
        traces,
        total,
        window_hours: hours,
    }))
}

/// GET /v1/llm/search
/// Full-text search on trace names and span error messages via SQLite FTS5.
/// Falls through to `traces_list` when no search query is provided.
pub async fn search(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<TraceListResponse>> {
    let search_query = match qp.search.as_deref() {
        Some(q) if !q.is_empty() => q.to_string(),
        _ => return traces_list(State(state), token_auth, Query(qp)).await,
    };

    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let project_id = qp.project_id.clone();
    let hours = qp.hours();
    let limit = qp.limit();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);

    // Step 1: FTS query in SQLite to get matching trace IDs
    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let fts_query = search_query;
    let fts_limit = limit;
    let trace_ids: Vec<String> = conn
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT t.id
                 FROM llm_traces_fts f
                 JOIN llm_traces t ON t.name = f.name
                 WHERE llm_traces_fts MATCH ?1
                   AND t.started_at >= ?2
                   AND (?3 IS NULL OR t.project_id = ?3)
                 LIMIT ?4",
            )?;
            let ids = stmt
                .query_map(
                    sqlite_params![fts_query, since, project_id, fts_limit],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(ids)
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
        .map_err(|e| {
            // FTS5 syntax errors (e.g. unmatched quotes) should return 400, not 500
            let msg = e.to_string();
            if msg.contains("fts5") || msg.contains("syntax") || msg.contains("parse") {
                AppError::Validation(format!("invalid search query: {msg}"))
            } else {
                AppError::Database(e)
            }
        })?;

    if trace_ids.is_empty() {
        return Ok(Json(TraceListResponse {
            traces: Vec::new(),
            total: 0,
            window_hours: hours,
        }));
    }

    // Step 2: Look up the matching traces in DuckDB by ID
    let traces = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let placeholders: String = trace_ids
                .iter()
                .enumerate()
                .map(|(i, _)| format!("${}", i + 1))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT t.id, t.session_id, t.name, t.status, t.total_tokens, t.cost_micros,
                        (SELECT COUNT(*) FROM bloop.llm_spans s WHERE s.trace_id = t.id AND s.project_id = t.project_id) AS span_count,
                        t.started_at, t.ended_at, t.prompt_name, t.prompt_version
                 FROM bloop.llm_traces t
                 WHERE t.id IN ({})
                 ORDER BY t.started_at DESC",
                placeholders
            );
            let mut stmt = conn.prepare(&sql)?;
            let param_values: Vec<Box<dyn duckdb::types::ToSql>> = trace_ids
                .into_iter()
                .map(|id| Box::new(id) as Box<dyn duckdb::types::ToSql>)
                .collect();
            let refs: Vec<&dyn duckdb::types::ToSql> =
                param_values.iter().map(|b| b.as_ref()).collect();
            let rows = stmt.query_map(refs.as_slice(), |row| {
                Ok(TraceListEntry {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    name: row.get(2)?,
                    status: row.get(3)?,
                    total_tokens: row.get(4)?,
                    cost_micros: row.get(5)?,
                    span_count: row.get(6)?,
                    started_at: row.get(7)?,
                    ended_at: row.get(8)?,
                    prompt_name: row.get(9)?,
                    prompt_version: row.get(10)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(AppError::LlmTracing)?;

    let total = traces.len() as i64;

    Ok(Json(TraceListResponse {
        traces,
        total,
        window_hours: hours,
    }))
}

/// GET /v1/llm/traces/{id} - Full trace + span hierarchy (reads from SQLite directly).
pub async fn trace_detail(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(trace_id): Path<String>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<TraceDetailResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let project_id = qp.project_id.clone();

    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let tid = trace_id.clone();
    let pid = project_id.clone();

    let result = conn
        .interact(move |conn| {
            // Get trace
            let trace = conn.query_row(
                "SELECT project_id, id, session_id, user_id, name, status,
                        input_tokens, output_tokens, total_tokens, cost_micros,
                        input, output, metadata, started_at, ended_at,
                        prompt_name, prompt_version
                 FROM llm_traces WHERE id = ?1 AND (?2 IS NULL OR project_id = ?2)",
                sqlite_params![tid, pid],
                |row| {
                    let metadata_str: Option<String> = row.get(12)?;
                    let metadata: Option<serde_json::Value> =
                        metadata_str.and_then(|s| serde_json::from_str(&s).ok());
                    let input: Option<String> = row.get(10)?;
                    let output: Option<String> = row.get(11)?;
                    Ok(TraceDetailResponse {
                        project_id: row.get(0)?,
                        id: row.get(1)?,
                        session_id: row.get(2)?,
                        user_id: row.get(3)?,
                        name: row.get(4)?,
                        status: row.get(5)?,
                        input_tokens: row.get(6)?,
                        output_tokens: row.get(7)?,
                        total_tokens: row.get(8)?,
                        cost_micros: row.get(9)?,
                        input,
                        output,
                        metadata,
                        started_at: row.get(13)?,
                        ended_at: row.get(14)?,
                        prompt_name: row.get(15)?,
                        prompt_version: row.get(16)?,
                        spans: Vec::new(),
                    })
                },
            )?;

            // Get spans
            let mut stmt = conn.prepare(
                "SELECT id, parent_span_id, span_type, name, model, provider,
                        input_tokens, output_tokens, total_tokens, cost_micros,
                        latency_ms, time_to_first_token_ms, status, error_message,
                        input, output, metadata, started_at, ended_at
                 FROM llm_spans WHERE trace_id = ?1 AND project_id = ?2
                 ORDER BY started_at ASC",
            )?;

            let spans: Vec<SpanDetail> = stmt
                .query_map(sqlite_params![trace.id, trace.project_id], |row| {
                    let metadata_str: Option<String> = row.get(16)?;
                    let metadata: Option<serde_json::Value> =
                        metadata_str.and_then(|s| serde_json::from_str(&s).ok());
                    Ok(SpanDetail {
                        id: row.get(0)?,
                        parent_span_id: row.get(1)?,
                        span_type: row.get(2)?,
                        name: row.get(3)?,
                        model: row.get(4)?,
                        provider: row.get(5)?,
                        input_tokens: row.get(6)?,
                        output_tokens: row.get(7)?,
                        total_tokens: row.get(8)?,
                        cost_micros: row.get(9)?,
                        latency_ms: row.get(10)?,
                        time_to_first_token_ms: row.get(11)?,
                        status: row.get(12)?,
                        error_message: row.get(13)?,
                        input: row.get(14)?,
                        output: row.get(15)?,
                        metadata,
                        started_at: row.get(17)?,
                        ended_at: row.get(18)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(TraceDetailResponse { spans, ..trace })
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
        .map_err(|e: rusqlite::Error| {
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                AppError::NotFound(format!("trace not found: {trace_id}"))
            } else {
                AppError::Database(e)
            }
        })?;

    Ok(Json(result))
}

/// GET /v1/llm/prompts
pub async fn prompts(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<PromptsResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let limit = qp.limit();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);
    let project_id = qp.project_id.clone();

    let prompts = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::PROMPTS_SQL)?;
            let rows = stmt.query_map(params![since, project_id, limit], |row| {
                Ok(PromptEntry {
                    name: row.get(0)?,
                    version_count: row.get(1)?,
                    total_traces: row.get(2)?,
                    avg_cost_micros: row.get::<_, f64>(3)? as i64,
                    error_rate: row.get(4)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(AppError::LlmTracing)?;

    Ok(Json(PromptsResponse {
        prompts,
        window_hours: hours,
    }))
}

/// GET /v1/llm/sessions
pub async fn sessions_list(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<SessionListResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let limit = qp.limit();
    let offset = qp.offset();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);
    let project_id = qp.project_id.clone();
    let user_id = qp.user_id.clone();
    let project_id2 = project_id.clone();
    let user_id2 = user_id.clone();

    let sessions = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::SESSIONS_LIST_SQL)?;
            let rows =
                stmt.query_map(params![since, project_id, user_id, limit, offset], |row| {
                    Ok(SessionEntry {
                        session_id: row.get(0)?,
                        user_id: row.get(1)?,
                        trace_count: row.get(2)?,
                        total_cost_micros: row.get(3)?,
                        total_tokens: row.get(4)?,
                        first_trace_at: row.get(5)?,
                        last_trace_at: row.get(6)?,
                        error_count: row.get(7)?,
                    })
                })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(AppError::LlmTracing)?;

    let total = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::SESSIONS_COUNT_SQL)?;
            stmt.query_row(params![since, project_id2, user_id2], |row| {
                row.get::<_, i64>(0)
            })
        })
        .await
        .map_err(AppError::LlmTracing)?;

    Ok(Json(SessionListResponse {
        sessions,
        total,
        window_hours: hours,
    }))
}

/// GET /v1/llm/sessions/{session_id}
pub async fn session_detail(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(session_id): Path<String>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<SessionDetailResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let project_id = qp.project_id.clone();
    let sid = session_id.clone();

    let traces = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::SESSION_DETAIL_SQL)?;
            let rows = stmt.query_map(params![sid, project_id], |row| {
                Ok(TraceListEntry {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    name: row.get(2)?,
                    status: row.get(3)?,
                    total_tokens: row.get(4)?,
                    cost_micros: row.get(5)?,
                    span_count: row.get(6)?,
                    started_at: row.get(7)?,
                    ended_at: row.get(8)?,
                    prompt_name: row.get(9)?,
                    prompt_version: row.get(10)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(AppError::LlmTracing)?;

    // Compute totals from the traces
    let total_cost_micros: i64 = traces.iter().map(|t| t.cost_micros).sum();
    let total_tokens: i64 = traces.iter().map(|t| t.total_tokens).sum();
    let user_id = traces.first().and({
        // user_id isn't on TraceListEntry, return None
        None
    });

    Ok(Json(SessionDetailResponse {
        session_id,
        user_id,
        traces,
        total_cost_micros,
        total_tokens,
    }))
}

/// GET /v1/llm/tools
pub async fn tools(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<ToolsResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let limit = qp.limit();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);
    let project_id = qp.project_id.clone();

    let tools = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::TOOLS_SQL)?;
            let rows = stmt.query_map(params![since, project_id, limit], |row| {
                Ok(ToolEntry {
                    name: row.get(0)?,
                    call_count: row.get(1)?,
                    error_count: row.get(2)?,
                    error_rate: row.get(3)?,
                    avg_latency_ms: row.get(4)?,
                    p50_latency_ms: row.get(5)?,
                    p99_latency_ms: row.get(6)?,
                    total_cost_micros: row.get(7)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(AppError::LlmTracing)?;

    Ok(Json(ToolsResponse {
        tools,
        window_hours: hours,
    }))
}

/// GET /v1/llm/prompts/{name}/versions
pub async fn prompt_versions(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(name): Path<String>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<PromptVersionsResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let limit = qp.limit();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);
    let project_id = qp.project_id.clone();
    let prompt_name = name.clone();

    let versions = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::PROMPT_VERSIONS_SQL)?;
            let rows = stmt.query_map(params![since, project_id, prompt_name, limit], |row| {
                Ok(PromptVersionEntry {
                    version: row.get(0)?,
                    trace_count: row.get(1)?,
                    avg_cost_micros: row.get::<_, f64>(2)? as i64,
                    avg_latency_ms: row.get(3)?,
                    error_rate: row.get(4)?,
                    avg_input_tokens: row.get(5)?,
                    avg_output_tokens: row.get(6)?,
                    first_seen: row.get(7)?,
                    last_seen: row.get(8)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(AppError::LlmTracing)?;

    Ok(Json(PromptVersionsResponse {
        name,
        versions,
        window_hours: hours,
    }))
}

/// GET /v1/llm/prompts/{name}/compare?v1=X&v2=Y
pub async fn prompt_compare(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(name): Path<String>,
    Query(params): Query<PromptCompareParams>,
) -> AppResult<Json<PromptComparisonResponse>> {
    let project_id = resolve_project(&token_auth, params.project_id)?;
    let hours = params.hours.unwrap_or(24).clamp(1, 720);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);

    let pid1 = project_id.clone();
    let name1 = name.clone();
    let v1_str = params.v1.clone();

    let v1 = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::PROMPT_VERSION_METRICS_SQL)?;
            stmt.query_row(params![since, pid1, name1, v1_str], |row| {
                Ok(PromptVersionEntry {
                    version: row.get(0)?,
                    trace_count: row.get(1)?,
                    avg_cost_micros: row.get::<_, f64>(2)? as i64,
                    avg_latency_ms: row.get(3)?,
                    error_rate: row.get(4)?,
                    avg_input_tokens: row.get(5)?,
                    avg_output_tokens: row.get(6)?,
                    first_seen: row.get(7)?,
                    last_seen: row.get(8)?,
                })
            })
        })
        .await
        .map_err(AppError::LlmTracing)?;

    let pid2 = project_id;
    let name2 = name.clone();
    let v2_str = params.v2.clone();

    let v2 = state
        .conn
        .query(move |conn: &duckdb::Connection| {
            let mut stmt = conn.prepare(query::PROMPT_VERSION_METRICS_SQL)?;
            stmt.query_row(params![since, pid2, name2, v2_str], |row| {
                Ok(PromptVersionEntry {
                    version: row.get(0)?,
                    trace_count: row.get(1)?,
                    avg_cost_micros: row.get::<_, f64>(2)? as i64,
                    avg_latency_ms: row.get(3)?,
                    error_rate: row.get(4)?,
                    avg_input_tokens: row.get(5)?,
                    avg_output_tokens: row.get(6)?,
                    first_seen: row.get(7)?,
                    last_seen: row.get(8)?,
                })
            })
        })
        .await
        .map_err(AppError::LlmTracing)?;

    Ok(Json(PromptComparisonResponse { name, v1, v2 }))
}

/// GET /v1/llm/settings
pub async fn get_settings(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<LlmQueryParams>,
) -> AppResult<Json<SettingsResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let project_id = qp.project_id.unwrap_or_else(|| "default".to_string());

    let policy = state
        .settings_cache
        .load(&project_id)
        .await
        .map_err(|e| AppError::LlmTracing(format!("settings load error: {e}")))?;

    Ok(Json(SettingsResponse {
        project_id,
        content_storage: policy.as_str().to_string(),
    }))
}

/// PUT /v1/llm/settings
pub async fn update_settings(
    State(state): State<Arc<LlmQueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<LlmQueryParams>,
    Json(update): Json<UpdateSettings>,
) -> AppResult<Json<SettingsResponse>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let project_id = qp.project_id.unwrap_or_else(|| "default".to_string());

    let policy = ContentStorage::from_str_loose(&update.content_storage);

    state
        .settings_cache
        .update(&project_id, policy.clone())
        .await
        .map_err(|e| AppError::LlmTracing(format!("settings update error: {e}")))?;

    Ok(Json(SettingsResponse {
        project_id,
        content_storage: policy.as_str().to_string(),
    }))
}
