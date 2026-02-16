use crate::auth::bearer::TokenAuth;
use crate::error::{AppError, AppResult};
use crate::llm_tracing::types::*;
use crate::query::handler::resolve_project_for_scope;
use axum::extract::{Path, Query, State};
use axum::Json;
use deadpool_sqlite::Pool;
use rusqlite::params;
use std::collections::HashMap;
use std::sync::Arc;

fn resolve_project(
    token_auth: &Option<axum::Extension<TokenAuth>>,
    query_project_id: Option<String>,
) -> AppResult<Option<String>> {
    resolve_project_for_scope(token_auth, query_project_id, "errors:read")
}

/// Compute the p-th percentile from a sorted slice.
/// `p` is in [0, 100]. Returns 0.0 for an empty slice.
pub fn percentile(sorted_values: &[f64], p: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    let idx = (p / 100.0 * (sorted_values.len() - 1) as f64).round() as usize;
    sorted_values[idx.min(sorted_values.len() - 1)]
}

/// POST /v1/llm/traces/{trace_id}/scores
pub async fn post_score(
    State(pool): State<Arc<Pool>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(trace_id): Path<String>,
    Json(input): Json<ScoreInput>,
) -> AppResult<Json<serde_json::Value>> {
    let project_id = resolve_project(&token_auth, None)?.unwrap_or_else(|| "default".to_string());

    // Validate name
    if input.name.is_empty() || input.name.len() > 64 {
        return Err(AppError::Validation("name must be 1-64 characters".into()));
    }
    if !input
        .name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AppError::Validation(
            "name must be alphanumeric, underscore, or hyphen".into(),
        ));
    }

    // Validate value
    if input.value < 0.0 || input.value > 1.0 {
        return Err(AppError::Validation(
            "value must be between 0.0 and 1.0".into(),
        ));
    }

    // Validate source
    let source = input.source.unwrap_or_else(|| "api".to_string());
    if !["api", "user", "auto"].contains(&source.as_str()) {
        return Err(AppError::Validation(
            "source must be api, user, or auto".into(),
        ));
    }

    // Validate comment
    if let Some(ref comment) = input.comment {
        if comment.len() > 500 {
            return Err(AppError::Validation(
                "comment must be at most 500 characters".into(),
            ));
        }
    }

    let now = chrono::Utc::now().timestamp_millis();
    let name = input.name;
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
            "INSERT INTO llm_trace_scores (project_id, trace_id, name, value, source, comment, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (project_id, trace_id, name) DO UPDATE SET
                value = excluded.value,
                source = excluded.source,
                comment = excluded.comment,
                created_at = excluded.created_at",
            params![project_id, trace_id, name, value, source, comment, now],
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

/// GET /v1/llm/traces/{trace_id}/scores
pub async fn get_scores(
    State(pool): State<Arc<Pool>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(trace_id): Path<String>,
) -> AppResult<Json<ScoresListResponse>> {
    let project_id = resolve_project(&token_auth, None)?.unwrap_or_else(|| "default".to_string());

    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let pid = project_id;
    let tid = trace_id;

    let scores = conn
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT name, value, source, comment, created_at
                 FROM llm_trace_scores WHERE project_id = ?1 AND trace_id = ?2
                 ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map(params![pid, tid], |row| {
                Ok(ScoreResponse {
                    name: row.get(0)?,
                    value: row.get(1)?,
                    source: row.get(2)?,
                    comment: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
        .map_err(AppError::Database)?;

    Ok(Json(ScoresListResponse { scores }))
}

/// GET /v1/llm/scores/summary
pub async fn get_score_summary(
    State(pool): State<Arc<Pool>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(params): Query<ScoreSummaryParams>,
) -> AppResult<Json<ScoreSummary>> {
    let project_id =
        resolve_project(&token_auth, params.project_id)?.unwrap_or_else(|| "default".to_string());
    let name = params.name.unwrap_or_else(|| "accuracy".to_string());
    let hours = params.hours.unwrap_or(24).clamp(1, 720);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);

    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let pid = project_id;
    let score_name = name.clone();
    let name_for_resp = name;

    let (values, count) = conn
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT value FROM llm_trace_scores
                 WHERE project_id = ?1 AND name = ?2 AND created_at >= ?3
                 ORDER BY value ASC",
            )?;
            let rows =
                stmt.query_map(params![pid, score_name, since], |row| row.get::<_, f64>(0))?;
            let values: Vec<f64> = rows.collect::<Result<Vec<_>, _>>()?;
            let count = values.len() as i64;
            Ok((values, count))
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
        .map_err(AppError::Database)?;

    let avg = if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    };

    let p10 = percentile(&values, 10.0);
    let p50 = percentile(&values, 50.0);
    let p90 = percentile(&values, 90.0);

    // Build distribution buckets: "0.0-0.2", "0.2-0.4", "0.4-0.6", "0.6-0.8", "0.8-1.0"
    let mut distribution: HashMap<String, i64> = HashMap::new();
    let buckets = [
        ("0.0-0.2", 0.0, 0.2),
        ("0.2-0.4", 0.2, 0.4),
        ("0.4-0.6", 0.4, 0.6),
        ("0.6-0.8", 0.6, 0.8),
        ("0.8-1.0", 0.8, 1.01), // 1.01 to include 1.0
    ];
    for (label, _, _) in &buckets {
        distribution.insert(label.to_string(), 0);
    }
    for v in &values {
        for (label, lo, hi) in &buckets {
            if *v >= *lo && *v < *hi {
                *distribution.entry(label.to_string()).or_insert(0) += 1;
                break;
            }
        }
    }

    Ok(Json(ScoreSummary {
        name: name_for_resp,
        count,
        avg,
        p50,
        p10,
        p90,
        distribution,
    }))
}
