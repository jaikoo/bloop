use crate::config::IngestConfig;
use crate::error::{AppError, AppResult};
use crate::fingerprint::compute_fingerprint;
use crate::ingest::auth::ProjectAuth;
use crate::types::{IngestEvent, ProcessedEvent};
use axum::extract::State;
use axum::{Extension, Json};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct IngestState {
    pub config: IngestConfig,
    pub tx: mpsc::Sender<ProcessedEvent>,
}

/// Validate an event against size limits.
fn validate_event(event: &IngestEvent, config: &IngestConfig) -> AppResult<()> {
    if event.message.len() > config.max_message_bytes {
        return Err(AppError::Validation(format!(
            "message exceeds {} bytes",
            config.max_message_bytes
        )));
    }
    if let Some(ref stack) = event.stack {
        if stack.len() > config.max_stack_bytes {
            return Err(AppError::Validation(format!(
                "stack exceeds {} bytes",
                config.max_stack_bytes
            )));
        }
    }
    if let Some(ref metadata) = event.metadata {
        let meta_str = metadata.to_string();
        if meta_str.len() > config.max_metadata_bytes {
            return Err(AppError::Validation(format!(
                "metadata exceeds {} bytes",
                config.max_metadata_bytes
            )));
        }
    }
    if event.environment.is_empty() {
        return Err(AppError::Validation("environment is required".to_string()));
    }
    if event.release.is_empty() {
        return Err(AppError::Validation("release is required".to_string()));
    }
    if event.error_type.is_empty() {
        return Err(AppError::Validation("error_type is required".to_string()));
    }
    if event.message.is_empty() {
        return Err(AppError::Validation("message is required".to_string()));
    }
    Ok(())
}

/// Process an event: validate, compute fingerprint, enqueue.
fn process_event(
    event: IngestEvent,
    config: &IngestConfig,
    project_id: &str,
) -> AppResult<ProcessedEvent> {
    validate_event(&event, config)?;

    let fingerprint = event.fingerprint.clone().unwrap_or_else(|| {
        compute_fingerprint(
            event.source.as_str(),
            &event.error_type,
            event.route_or_procedure.as_deref(),
            &event.message,
            event.stack.as_deref(),
        )
    });

    let received_at = chrono::Utc::now().timestamp_millis();

    Ok(ProcessedEvent {
        event,
        fingerprint,
        received_at,
        project_id: project_id.to_string(),
    })
}

/// POST /v1/ingest - Submit a single error event.
pub async fn ingest_single(
    State(state): State<Arc<IngestState>>,
    project_auth: Option<Extension<ProjectAuth>>,
    Json(event): Json<IngestEvent>,
) -> AppResult<Json<serde_json::Value>> {
    let project_id = project_auth
        .map(|Extension(a)| a.project_id)
        .unwrap_or_else(|| "default".to_string());

    let processed = process_event(event, &state.config, &project_id)?;

    // Backpressure: try_send, ACK 200 even if dropped
    if state.tx.try_send(processed).is_err() {
        tracing::warn!("channel full, event dropped");
    }

    Ok(Json(serde_json::json!({ "status": "accepted" })))
}

#[derive(Deserialize)]
pub struct BatchPayload {
    pub events: Vec<IngestEvent>,
}

/// POST /v1/ingest/batch - Submit a batch of events (max 50).
pub async fn ingest_batch(
    State(state): State<Arc<IngestState>>,
    project_auth: Option<Extension<ProjectAuth>>,
    Json(payload): Json<BatchPayload>,
) -> AppResult<Json<serde_json::Value>> {
    let project_id = project_auth
        .map(|Extension(a)| a.project_id)
        .unwrap_or_else(|| "default".to_string());

    if payload.events.len() > state.config.max_batch_size {
        return Err(AppError::Validation(format!(
            "batch exceeds max size of {}",
            state.config.max_batch_size
        )));
    }

    let mut accepted = 0u64;
    let mut dropped = 0u64;
    let mut errors = Vec::new();

    for (i, event) in payload.events.into_iter().enumerate() {
        match process_event(event, &state.config, &project_id) {
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

    Ok(Json(serde_json::json!({
        "accepted": accepted,
        "dropped": dropped,
        "errors": errors,
    })))
}
