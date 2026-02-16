use crate::config::LlmTracingConfig;
use crate::llm_tracing::types::ProcessedTrace;
use crate::llm_tracing::writer;
use deadpool_sqlite::Pool;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;

/// LLM tracing pipeline worker: consumes traces from the channel, batches them,
/// and flushes to SQLite on batch size or time trigger.
pub async fn run_worker(
    mut rx: mpsc::Receiver<ProcessedTrace>,
    pool: Pool,
    config: LlmTracingConfig,
) {
    let mut buffer: Vec<ProcessedTrace> = Vec::with_capacity(config.flush_batch_size);
    let flush_interval = Duration::from_secs(config.flush_interval_secs);
    let mut flush_timer = time::interval(flush_interval);
    flush_timer.tick().await; // skip first immediate tick

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Some(trace) => {
                        buffer.push(trace);
                        if buffer.len() >= config.flush_batch_size {
                            flush(&pool, &mut buffer).await;
                        }
                    }
                    None => {
                        // Channel closed — drain remaining
                        tracing::info!("llm tracing channel closed, draining buffer");
                        if !buffer.is_empty() {
                            flush(&pool, &mut buffer).await;
                        }
                        return;
                    }
                }
            }
            _ = flush_timer.tick() => {
                if !buffer.is_empty() {
                    flush(&pool, &mut buffer).await;
                }
            }
        }
    }
}

async fn flush(pool: &Pool, buffer: &mut Vec<ProcessedTrace>) {
    let mut traces: Vec<ProcessedTrace> = std::mem::take(buffer);
    let count = traces.len();

    for attempt in 0..2u8 {
        match writer::write_batch(pool, traces.clone()).await {
            Ok(_) => return,
            Err(e) => {
                if attempt == 0 {
                    tracing::warn!(error = %e, count, "llm flush failed, retrying in 500ms");
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                } else {
                    tracing::error!(error = %e, count, "llm flush retry failed — {count} traces dropped");
                }
            }
        }
    }
    traces.clear();
}
