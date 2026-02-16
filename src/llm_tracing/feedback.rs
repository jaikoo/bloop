use crate::auth::bearer::TokenAuth;
use crate::error::{AppError, AppResult};
use crate::llm_tracing::types::*;
use crate::query::handler::resolve_project_for_scope;
use axum::extract::{Path, Query, State};
use axum::Json;
use deadpool_sqlite::Pool;
use rusqlite::params;
use std::sync::Arc;

fn resolve_project(
    token_auth: &Option<axum::Extension<TokenAuth>>,
    query_project_id: Option<String>,
) -> AppResult<Option<String>> {
    resolve_project_for_scope(token_auth, query_project_id, "errors:read")
}

/// POST /v1/llm/traces/{trace_id}/feedback
pub async fn post_feedback(
    State(pool): State<Arc<Pool>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(trace_id): Path<String>,
    Json(input): Json<FeedbackInput>,
) -> AppResult<Json<serde_json::Value>> {
    let project_id = resolve_project(&token_auth, None)?.unwrap_or_else(|| "default".to_string());

    // Validate value: must be 1 or -1
    if input.value != 1 && input.value != -1 {
        return Err(AppError::Validation(
            "value must be 1 (positive) or -1 (negative)".into(),
        ));
    }

    // Validate comment length
    if let Some(ref comment) = input.comment {
        if comment.len() > 500 {
            return Err(AppError::Validation(
                "comment must be at most 500 characters".into(),
            ));
        }
    }

    let user_id = input.user_id.unwrap_or_else(|| "anonymous".to_string());
    let now = chrono::Utc::now().timestamp_millis();
    let value = input.value;
    let comment = input.comment;
    let tid = trace_id.clone();
    let pid = project_id.clone();
    let trace_id_for_err = trace_id.clone();

    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    conn.interact(move |conn| {
        // Verify trace exists
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM llm_traces WHERE project_id = ?1 AND id = ?2",
                params![pid, tid],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !exists {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }

        conn.execute(
            "INSERT INTO llm_trace_feedback (project_id, trace_id, user_id, value, comment, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (project_id, trace_id, user_id) DO UPDATE SET
                value = excluded.value,
                comment = excluded.comment,
                created_at = excluded.created_at",
            params![project_id, trace_id, user_id, value, comment, now],
        )?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
    .map_err(|e: rusqlite::Error| {
        if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
            AppError::NotFound(format!("trace not found: {}", trace_id_for_err))
        } else {
            AppError::Database(e)
        }
    })?;

    Ok(Json(serde_json::json!({ "status": "ok" })))
}

/// GET /v1/llm/traces/{trace_id}/feedback
pub async fn get_feedback(
    State(pool): State<Arc<Pool>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(trace_id): Path<String>,
) -> AppResult<Json<FeedbackListResponse>> {
    let project_id = resolve_project(&token_auth, None)?.unwrap_or_else(|| "default".to_string());

    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let pid = project_id;
    let tid = trace_id;

    let feedback = conn
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT trace_id, user_id, value, comment, created_at
                 FROM llm_trace_feedback WHERE project_id = ?1 AND trace_id = ?2
                 ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map(params![pid, tid], |row| {
                Ok(FeedbackResponse {
                    trace_id: row.get(0)?,
                    user_id: row.get(1)?,
                    value: row.get(2)?,
                    comment: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
        .map_err(AppError::Database)?;

    Ok(Json(FeedbackListResponse { feedback }))
}

/// GET /v1/llm/feedback/summary
pub async fn get_feedback_summary(
    State(pool): State<Arc<Pool>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(params): Query<FeedbackSummaryParams>,
) -> AppResult<Json<FeedbackSummary>> {
    let project_id =
        resolve_project(&token_auth, params.project_id)?.unwrap_or_else(|| "default".to_string());
    let hours = params.hours.unwrap_or(24).clamp(1, 720);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);

    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let pid = project_id;

    let (positive, negative) = conn
        .interact(move |conn| {
            let positive: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM llm_trace_feedback
                     WHERE project_id = ?1 AND value = 1 AND created_at >= ?2",
                    params![pid, since],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            let pid2 = pid;
            let negative: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM llm_trace_feedback
                     WHERE project_id = ?1 AND value = -1 AND created_at >= ?2",
                    params![pid2, since],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            Ok::<_, rusqlite::Error>((positive, negative))
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
        .map_err(AppError::Database)?;

    let total = positive + negative;
    let positive_rate = if total > 0 {
        positive as f64 * 100.0 / total as f64
    } else {
        0.0
    };

    Ok(Json(FeedbackSummary {
        total,
        positive,
        negative,
        positive_rate,
        window_hours: hours,
    }))
}
