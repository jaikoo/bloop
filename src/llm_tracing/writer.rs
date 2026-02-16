use crate::llm_tracing::types::ProcessedTrace;
use deadpool_sqlite::Pool;
use rusqlite::params;

/// Batch-write LLM traces + spans + hourly upserts in a single transaction.
pub async fn write_batch(
    pool: &Pool,
    traces: Vec<ProcessedTrace>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if traces.is_empty() {
        return Ok(());
    }

    let conn = pool.get().await?;
    conn.interact(move |conn| {
        let tx = conn.transaction()?;

        {
            let mut insert_trace = tx.prepare_cached(
                "INSERT OR REPLACE INTO llm_traces (
                    project_id, id, session_id, user_id, name, status,
                    input_tokens, output_tokens, total_tokens, cost_micros,
                    input, output, metadata,
                    started_at, ended_at, created_at,
                    prompt_name, prompt_version
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            )?;

            let mut insert_span = tx.prepare_cached(
                "INSERT OR REPLACE INTO llm_spans (
                    project_id, id, trace_id, parent_span_id, span_type, name,
                    model, provider,
                    input_tokens, output_tokens, total_tokens, cost_micros,
                    latency_ms, time_to_first_token_ms,
                    status, error_message,
                    input, output, metadata,
                    started_at, ended_at
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
            )?;

            let mut upsert_hourly = tx.prepare_cached(
                "INSERT INTO llm_usage_hourly (
                    project_id, hour_bucket, model, provider,
                    span_count, input_tokens, output_tokens, total_tokens,
                    cost_micros, error_count, total_latency_ms
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                ON CONFLICT (project_id, hour_bucket, model, provider) DO UPDATE SET
                    span_count = span_count + excluded.span_count,
                    input_tokens = input_tokens + excluded.input_tokens,
                    output_tokens = output_tokens + excluded.output_tokens,
                    total_tokens = total_tokens + excluded.total_tokens,
                    cost_micros = cost_micros + excluded.cost_micros,
                    error_count = error_count + excluded.error_count,
                    total_latency_ms = total_latency_ms + excluded.total_latency_ms",
            )?;

            let mut insert_fts = tx.prepare_cached(
                "INSERT INTO llm_traces_fts(rowid, name, error_messages) VALUES (?1, ?2, ?3)",
            )?;

            for trace in &traces {
                insert_trace.execute(params![
                    trace.project_id,
                    trace.id,
                    trace.session_id,
                    trace.user_id,
                    trace.name,
                    trace.status,
                    trace.input_tokens,
                    trace.output_tokens,
                    trace.total_tokens,
                    trace.cost_micros,
                    trace.input,
                    trace.output,
                    trace.metadata,
                    trace.started_at,
                    trace.ended_at,
                    trace.created_at,
                    trace.prompt_name,
                    trace.prompt_version,
                ])?;

                // Populate FTS index with trace name + span error messages
                let fts_rowid = {
                    use std::hash::{Hash, Hasher};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    trace.project_id.hash(&mut hasher);
                    trace.id.hash(&mut hasher);
                    hasher.finish() as i64
                };

                let error_messages: String = trace
                    .spans
                    .iter()
                    .filter_map(|s| s.error_message.as_deref())
                    .collect::<Vec<_>>()
                    .join(" ");

                // Best-effort FTS insert (ignore duplicates on re-ingest)
                let _ = insert_fts.execute(params![fts_rowid, &trace.name, &error_messages]);

                for span in &trace.spans {
                    insert_span.execute(params![
                        trace.project_id,
                        span.id,
                        span.trace_id,
                        span.parent_span_id,
                        span.span_type,
                        span.name,
                        span.model,
                        span.provider,
                        span.input_tokens,
                        span.output_tokens,
                        span.total_tokens,
                        span.cost_micros,
                        span.latency_ms,
                        span.time_to_first_token_ms,
                        span.status,
                        span.error_message,
                        span.input,
                        span.output,
                        span.metadata,
                        span.started_at,
                        span.ended_at,
                    ])?;

                    // Hourly usage aggregation
                    let hour_bucket = (span.started_at / 3_600_000) * 3_600_000;
                    let model = span.model.as_deref().unwrap_or("");
                    let provider = span.provider.as_deref().unwrap_or("");
                    let is_error = if span.status == "error" { 1i64 } else { 0i64 };

                    upsert_hourly.execute(params![
                        trace.project_id,
                        hour_bucket,
                        model,
                        provider,
                        1i64,
                        span.input_tokens,
                        span.output_tokens,
                        span.total_tokens,
                        span.cost_micros,
                        is_error,
                        span.latency_ms,
                    ])?;
                }
            }
        }

        tx.commit()?;
        tracing::debug!(count = traces.len(), "flushed llm trace batch to sqlite");
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| format!("interact error: {e}"))??;

    Ok(())
}
