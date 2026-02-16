use crate::config::PipelineConfig;
use crate::pipeline::aggregator::SharedAggregator;
use crate::storage::writer;
use crate::types::ProcessedEvent;
use deadpool_sqlite::Pool;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;

/// Pipeline worker: consumes events from the channel, batches them,
/// and flushes to SQLite on batch size or time trigger.
pub async fn run_worker(
    mut rx: mpsc::Receiver<ProcessedEvent>,
    pool: Pool,
    aggregator: SharedAggregator,
    config: PipelineConfig,
    new_fingerprint_tx: mpsc::UnboundedSender<NewFingerprintEvent>,
) {
    let mut buffer: Vec<ProcessedEvent> = Vec::with_capacity(config.flush_batch_size);
    let flush_interval = Duration::from_secs(config.flush_interval_secs);
    let mut flush_timer = time::interval(flush_interval);
    flush_timer.tick().await; // skip first immediate tick

    loop {
        tokio::select! {
            // Receive events from channel
            result = rx.recv() => {
                match result {
                    Some(event) => {
                        // Update in-memory aggregates
                        let is_new = aggregator.increment(&event.fingerprint, event.received_at);
                        if is_new {
                            let _ = new_fingerprint_tx.send(NewFingerprintEvent {
                                project_id: event.project_id.clone(),
                                fingerprint: event.fingerprint.clone(),
                                error_type: event.event.error_type.clone(),
                                message: event.event.message.clone(),
                                release: event.event.release.clone(),
                                environment: event.event.environment.clone(),
                            });
                        }

                        buffer.push(event);

                        // Flush on batch size
                        if buffer.len() >= config.flush_batch_size {
                            flush(&pool, &mut buffer, config.sample_reservoir_size).await;
                        }
                    }
                    None => {
                        // Channel closed - drain remaining
                        tracing::info!("channel closed, draining buffer");
                        if !buffer.is_empty() {
                            flush(&pool, &mut buffer, config.sample_reservoir_size).await;
                        }
                        return;
                    }
                }
            }
            // Flush on time interval
            _ = flush_timer.tick() => {
                if !buffer.is_empty() {
                    flush(&pool, &mut buffer, config.sample_reservoir_size).await;
                }
            }
        }
    }
}

async fn flush(pool: &Pool, buffer: &mut Vec<ProcessedEvent>, sample_reservoir_size: usize) {
    let mut events: Vec<ProcessedEvent> = std::mem::take(buffer);
    let count = events.len();

    // Try up to 2 times (initial + 1 retry) before dropping events.
    // ProcessedEvent must be Clone for retry (derived on the type).
    for attempt in 0..2u8 {
        match writer::write_batch(pool, events.clone(), sample_reservoir_size).await {
            Ok(_) => return,
            Err(e) => {
                if attempt == 0 {
                    tracing::warn!(error = %e, count, "flush failed, retrying in 500ms");
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                } else {
                    tracing::error!(error = %e, count, "flush retry failed — {count} events dropped");
                }
            }
        }
    }
    // Clear events if both attempts failed (they're already drained from buffer)
    events.clear();
}

/// Event emitted when a new fingerprint is first seen.
#[derive(Debug, Clone)]
pub struct NewFingerprintEvent {
    pub project_id: String,
    pub fingerprint: String,
    pub error_type: String,
    pub message: String,
    pub release: String,
    pub environment: String,
}
