/// LLM Overview: total traces, spans, tokens, cost, error rate.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL)
pub const OVERVIEW_SQL: &str = r#"
SELECT
    (SELECT COUNT(*) FROM bloop.llm_traces
     WHERE started_at >= $1 AND ($2 IS NULL OR project_id = $2)) AS total_traces,
    COALESCE(SUM(span_count), 0) AS total_spans,
    COALESCE(SUM(input_tokens), 0) AS total_input_tokens,
    COALESCE(SUM(output_tokens), 0) AS total_output_tokens,
    COALESCE(SUM(total_tokens), 0) AS total_tokens,
    COALESCE(SUM(cost_micros), 0) AS total_cost_micros,
    COALESCE(SUM(error_count), 0) AS error_count
FROM bloop.llm_usage_hourly
WHERE hour_bucket >= $1
    AND ($2 IS NULL OR project_id = $2)
"#;

/// Hourly usage breakdown by model/provider.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = model (or NULL),
///             $4 = provider (or NULL), $5 = limit
pub const USAGE_SQL: &str = r#"
SELECT
    hour_bucket,
    model,
    provider,
    span_count,
    input_tokens,
    output_tokens,
    total_tokens,
    cost_micros
FROM bloop.llm_usage_hourly
WHERE hour_bucket >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND ($3 IS NULL OR model = $3)
    AND ($4 IS NULL OR provider = $4)
ORDER BY hour_bucket DESC
LIMIT $5
"#;

/// Latency percentiles by model (p50/p90/p99 + avg TTFT).
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = model (or NULL)
pub const LATENCY_SQL: &str = r#"
SELECT
    COALESCE(model, '') AS model,
    PERCENTILE_CONT(0.50) WITHIN GROUP (ORDER BY latency_ms) AS p50_ms,
    PERCENTILE_CONT(0.90) WITHIN GROUP (ORDER BY latency_ms) AS p90_ms,
    PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY latency_ms) AS p99_ms,
    COALESCE(AVG(time_to_first_token_ms), 0) AS avg_ttft_ms,
    COUNT(*) AS span_count
FROM bloop.llm_spans
WHERE started_at >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND ($3 IS NULL OR model = $3)
    AND span_type = 'generation'
GROUP BY model
ORDER BY span_count DESC
"#;

/// Per-model breakdown: calls, tokens, cost, error rate, avg latency.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = limit
pub const MODELS_SQL: &str = r#"
SELECT
    model,
    provider,
    SUM(span_count) AS span_count,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    SUM(total_tokens) AS total_tokens,
    SUM(cost_micros) AS cost_micros,
    SUM(error_count) AS error_count,
    CASE WHEN SUM(span_count) > 0
        THEN (SUM(error_count) * 100.0 / SUM(span_count))
        ELSE 0.0 END AS error_rate,
    CASE WHEN SUM(span_count) > 0
        THEN (SUM(total_latency_ms) * 1.0 / SUM(span_count))
        ELSE 0.0 END AS avg_latency_ms
FROM bloop.llm_usage_hourly
WHERE hour_bucket >= $1
    AND ($2 IS NULL OR project_id = $2)
GROUP BY model, provider
ORDER BY SUM(cost_micros) DESC
LIMIT $3
"#;

/// Paginated trace list with span count and extended filters.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = session_id (or NULL),
///             $4 = status (or NULL), $5 = user_id (or NULL), $6 = min_cost_micros (or NULL),
///             $7 = sort_key, $8 = limit, $9 = offset,
///             $10 = prompt_name (or NULL), $11 = prompt_version (or NULL)
pub const TRACES_LIST_SQL: &str = r#"
SELECT
    t.id,
    t.session_id,
    t.name,
    t.status,
    t.total_tokens,
    t.cost_micros,
    (SELECT COUNT(*) FROM bloop.llm_spans s WHERE s.trace_id = t.id AND s.project_id = t.project_id) AS span_count,
    t.started_at,
    t.ended_at,
    t.prompt_name,
    t.prompt_version
FROM bloop.llm_traces t
WHERE t.started_at >= $1
    AND ($2 IS NULL OR t.project_id = $2)
    AND ($3 IS NULL OR t.session_id = $3)
    AND ($4 IS NULL OR t.status = $4)
    AND ($5 IS NULL OR t.user_id = $5)
    AND ($6 IS NULL OR t.cost_micros >= $6)
    AND ($10 IS NULL OR t.prompt_name = $10)
    AND ($11 IS NULL OR t.prompt_version = $11)
ORDER BY
    CASE WHEN $7 = 'oldest' THEN t.started_at END ASC,
    CASE WHEN $7 = 'most_expensive' THEN t.cost_micros END DESC,
    CASE WHEN $7 = 'slowest' THEN (t.ended_at - t.started_at) END DESC,
    t.started_at DESC
LIMIT $8 OFFSET $9
"#;

/// Total trace count for pagination (with extended filters).
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = session_id (or NULL),
///             $4 = status (or NULL), $5 = user_id (or NULL), $6 = min_cost_micros (or NULL),
///             $7 = prompt_name (or NULL), $8 = prompt_version (or NULL)
pub const TRACES_COUNT_SQL: &str = r#"
SELECT COUNT(*)
FROM bloop.llm_traces
WHERE started_at >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND ($3 IS NULL OR session_id = $3)
    AND ($4 IS NULL OR status = $4)
    AND ($5 IS NULL OR user_id = $5)
    AND ($6 IS NULL OR cost_micros >= $6)
    AND ($7 IS NULL OR prompt_name = $7)
    AND ($8 IS NULL OR prompt_version = $8)
"#;

/// Session list: GROUP BY session_id with aggregates.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = user_id (or NULL),
///             $4 = limit, $5 = offset
pub const SESSIONS_LIST_SQL: &str = r#"
SELECT
    session_id,
    MIN(user_id) AS user_id,
    COUNT(*) AS trace_count,
    COALESCE(SUM(cost_micros), 0) AS total_cost_micros,
    COALESCE(SUM(total_tokens), 0) AS total_tokens,
    MIN(started_at) AS first_trace_at,
    MAX(started_at) AS last_trace_at,
    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS error_count
FROM bloop.llm_traces
WHERE started_at >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND ($3 IS NULL OR user_id = $3)
    AND session_id IS NOT NULL
GROUP BY session_id
ORDER BY MAX(started_at) DESC
LIMIT $4 OFFSET $5
"#;

/// Count distinct sessions for pagination.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = user_id (or NULL)
pub const SESSIONS_COUNT_SQL: &str = r#"
SELECT COUNT(DISTINCT session_id)
FROM bloop.llm_traces
WHERE started_at >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND ($3 IS NULL OR user_id = $3)
    AND session_id IS NOT NULL
"#;

/// All traces in a session ordered by started_at ASC.
/// Parameters: $1 = session_id, $2 = project_id (or NULL)
pub const SESSION_DETAIL_SQL: &str = r#"
SELECT
    t.id,
    t.session_id,
    t.name,
    t.status,
    t.total_tokens,
    t.cost_micros,
    (SELECT COUNT(*) FROM bloop.llm_spans s WHERE s.trace_id = t.id AND s.project_id = t.project_id) AS span_count,
    t.started_at,
    t.ended_at,
    t.prompt_name,
    t.prompt_version
FROM bloop.llm_traces t
WHERE t.session_id = $1
    AND ($2 IS NULL OR t.project_id = $2)
ORDER BY t.started_at ASC
"#;

/// Tool/retrieval span analytics: GROUP BY name WHERE span_type IN ('tool','retrieval').
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = limit
pub const TOOLS_SQL: &str = r#"
SELECT
    name,
    COUNT(*) AS call_count,
    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS error_count,
    CASE WHEN COUNT(*) > 0
        THEN (SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) * 100.0 / COUNT(*))
        ELSE 0.0 END AS error_rate,
    COALESCE(AVG(latency_ms), 0) AS avg_latency_ms,
    COALESCE(PERCENTILE_CONT(0.50) WITHIN GROUP (ORDER BY latency_ms), 0) AS p50_latency_ms,
    COALESCE(PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY latency_ms), 0) AS p99_latency_ms,
    COALESCE(SUM(cost_micros), 0) AS total_cost_micros
FROM bloop.llm_spans
WHERE started_at >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND span_type IN ('tool', 'retrieval')
GROUP BY name
ORDER BY call_count DESC
LIMIT $3
"#;

/// Per-version metrics for a given prompt_name.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = prompt_name, $4 = limit
pub const PROMPT_VERSIONS_SQL: &str = r#"
SELECT
    prompt_version,
    COUNT(*) AS trace_count,
    COALESCE(AVG(cost_micros), 0) AS avg_cost_micros,
    COALESCE(AVG(CASE WHEN ended_at IS NOT NULL AND started_at IS NOT NULL
        THEN ended_at - started_at ELSE 0 END), 0) AS avg_latency_ms,
    CASE WHEN COUNT(*) > 0
        THEN (SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) * 1.0 / COUNT(*))
        ELSE 0.0 END AS error_rate,
    COALESCE(AVG(input_tokens), 0) AS avg_input_tokens,
    COALESCE(AVG(output_tokens), 0) AS avg_output_tokens,
    MIN(started_at) AS first_seen,
    MAX(started_at) AS last_seen
FROM bloop.llm_traces
WHERE started_at >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND prompt_name = $3
    AND prompt_version IS NOT NULL
GROUP BY prompt_version
ORDER BY MAX(started_at) DESC
LIMIT $4
"#;

/// Single prompt version metrics (used for comparison).
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = prompt_name, $4 = prompt_version
pub const PROMPT_VERSION_METRICS_SQL: &str = r#"
SELECT
    prompt_version,
    COUNT(*) AS trace_count,
    COALESCE(AVG(cost_micros), 0) AS avg_cost_micros,
    COALESCE(AVG(CASE WHEN ended_at IS NOT NULL AND started_at IS NOT NULL
        THEN ended_at - started_at ELSE 0 END), 0) AS avg_latency_ms,
    CASE WHEN COUNT(*) > 0
        THEN (SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) * 1.0 / COUNT(*))
        ELSE 0.0 END AS error_rate,
    COALESCE(AVG(input_tokens), 0) AS avg_input_tokens,
    COALESCE(AVG(output_tokens), 0) AS avg_output_tokens,
    MIN(started_at) AS first_seen,
    MAX(started_at) AS last_seen
FROM bloop.llm_traces
WHERE started_at >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND prompt_name = $3
    AND prompt_version = $4
"#;

/// Per-prompt summary: version count, total traces, avg cost, error rate.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = limit
pub const PROMPTS_SQL: &str = r#"
SELECT
    prompt_name,
    COUNT(DISTINCT prompt_version) AS version_count,
    COUNT(*) AS total_traces,
    COALESCE(AVG(cost_micros), 0) AS avg_cost_micros,
    CASE WHEN COUNT(*) > 0
        THEN (SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) * 1.0 / COUNT(*))
        ELSE 0.0 END AS error_rate
FROM bloop.llm_traces
WHERE started_at >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND prompt_name IS NOT NULL
GROUP BY prompt_name
ORDER BY total_traces DESC
LIMIT $3
"#;

/// RAG source overview: per-source aggregates for retrieval spans with rag.* metadata.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = limit
pub const RAG_OVERVIEW_SQL: &str = r#"
SELECT
    COALESCE(name, '') AS retrieval_name,
    COALESCE(json_extract_string(metadata, '$."rag.source"'), 'unknown') AS source,
    COUNT(*) AS call_count,
    COALESCE(AVG(latency_ms), 0) AS avg_latency_ms,
    COALESCE(PERCENTILE_CONT(0.50) WITHIN GROUP (ORDER BY latency_ms), 0) AS p50_latency_ms,
    COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY latency_ms), 0) AS p95_latency_ms,
    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS error_count,
    CASE WHEN COUNT(*) > 0
        THEN (SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) * 100.0 / COUNT(*))
        ELSE 0.0 END AS error_rate,
    COALESCE(AVG(CAST(json_extract(metadata, '$."rag.chunks_retrieved"') AS DOUBLE)), 0) AS avg_chunks_retrieved,
    COALESCE(AVG(CAST(json_extract(metadata, '$."rag.chunks_used"') AS DOUBLE)), 0) AS avg_chunks_used,
    COALESCE(AVG(CAST(json_extract(metadata, '$."rag.context_tokens"') AS DOUBLE)), 0) AS avg_context_tokens,
    COALESCE(AVG(
        CASE WHEN CAST(json_extract(metadata, '$."rag.max_context_tokens"') AS DOUBLE) > 0
            THEN CAST(json_extract(metadata, '$."rag.context_tokens"') AS DOUBLE) * 100.0
                 / CAST(json_extract(metadata, '$."rag.max_context_tokens"') AS DOUBLE)
            ELSE NULL END
    ), 0) AS avg_context_utilization_pct
FROM bloop.llm_spans
WHERE started_at >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND span_type = 'retrieval'
    AND metadata IS NOT NULL
    AND json_extract(metadata, '$."rag.chunks_retrieved"') IS NOT NULL
GROUP BY name, json_extract_string(metadata, '$."rag.source"')
ORDER BY call_count DESC
LIMIT $3
"#;

/// RAG hourly metrics: time-series for retrieval span activity.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL)
pub const RAG_METRICS_SQL: &str = r#"
SELECT
    (started_at / 3600000) * 3600000 AS hour_bucket,
    COUNT(*) AS retrieval_count,
    COALESCE(AVG(latency_ms), 0) AS avg_latency_ms,
    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS error_count,
    COALESCE(AVG(CAST(json_extract(metadata, '$."rag.chunks_retrieved"') AS DOUBLE)), 0) AS avg_chunks_retrieved,
    COALESCE(AVG(CAST(json_extract(metadata, '$."rag.context_tokens"') AS DOUBLE)), 0) AS avg_context_tokens,
    COALESCE(AVG(
        CASE WHEN CAST(json_extract(metadata, '$."rag.max_context_tokens"') AS DOUBLE) > 0
            THEN CAST(json_extract(metadata, '$."rag.context_tokens"') AS DOUBLE) * 100.0
                 / CAST(json_extract(metadata, '$."rag.max_context_tokens"') AS DOUBLE)
            ELSE NULL END
    ), 0) AS avg_context_utilization_pct,
    COALESCE(AVG(CAST(json_extract(metadata, '$."rag.top_k"') AS DOUBLE)), 0) AS avg_top_k
FROM bloop.llm_spans
WHERE started_at >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND span_type = 'retrieval'
    AND metadata IS NOT NULL
    AND json_extract(metadata, '$."rag.chunks_retrieved"') IS NOT NULL
GROUP BY (started_at / 3600000) * 3600000
ORDER BY hour_bucket DESC
"#;

/// RAG relevance summary: aggregate relevance score stats across retrieval spans.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL)
pub const RAG_RELEVANCE_SQL: &str = r#"
SELECT
    COUNT(*) AS total_retrievals,
    COALESCE(AVG(CAST(json_extract(metadata, '$."rag.relevance_scores"[0]') AS DOUBLE)), 0) AS avg_top_relevance,
    COALESCE(MIN(CAST(json_extract(metadata, '$."rag.relevance_scores"[0]') AS DOUBLE)), 0) AS min_top_relevance,
    COALESCE(MAX(CAST(json_extract(metadata, '$."rag.relevance_scores"[0]') AS DOUBLE)), 0) AS max_top_relevance,
    COALESCE(AVG(CAST(json_extract(metadata, '$."rag.chunks_retrieved"') AS DOUBLE)), 0) AS avg_chunks_retrieved,
    COALESCE(AVG(CAST(json_extract(metadata, '$."rag.chunks_used"') AS DOUBLE)), 0) AS avg_chunks_used,
    COALESCE(AVG(
        CASE WHEN CAST(json_extract(metadata, '$."rag.chunks_retrieved"') AS DOUBLE) > 0
            THEN CAST(json_extract(metadata, '$."rag.chunks_used"') AS DOUBLE) * 100.0
                 / CAST(json_extract(metadata, '$."rag.chunks_retrieved"') AS DOUBLE)
            ELSE NULL END
    ), 0) AS chunk_utilization_pct
FROM bloop.llm_spans
WHERE started_at >= $1
    AND ($2 IS NULL OR project_id = $2)
    AND span_type = 'retrieval'
    AND metadata IS NOT NULL
    AND json_extract(metadata, '$."rag.chunks_retrieved"') IS NOT NULL
"#;
