use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use deadpool_sqlite::Pool;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::auth::session::SessionUser;

/// Shared state for retention API routes.
pub struct RetentionState {
    pub pool: Pool,
    pub default_raw_events_days: u64,
}

#[derive(Serialize)]
pub struct RetentionResponse {
    pub global: GlobalRetention,
    pub per_project: Vec<ProjectRetention>,
    pub storage: StorageStats,
}

#[derive(Serialize)]
pub struct GlobalRetention {
    pub raw_events_days: u64,
    pub hourly_counts_days: u64,
}

#[derive(Serialize)]
pub struct ProjectRetention {
    pub project_id: String,
    pub project_name: String,
    pub raw_events_days: u64,
}

#[derive(Serialize)]
pub struct StorageStats {
    pub db_size_bytes: i64,
    pub raw_events_count: i64,
    pub hourly_counts_count: i64,
}

#[derive(Deserialize)]
pub struct UpdateRetentionRequest {
    pub raw_events_days: Option<u64>,
    pub hourly_counts_days: Option<u64>,
    #[serde(default)]
    pub per_project: Vec<ProjectRetentionUpdate>,
}

#[derive(Deserialize)]
pub struct ProjectRetentionUpdate {
    pub project_id: String,
    pub raw_events_days: u64,
}

#[derive(Deserialize)]
pub struct PurgeRequest {
    pub confirm: bool,
}

#[allow(clippy::result_large_err)]
fn require_admin(session_user: &SessionUser) -> Result<(), Response> {
    if !session_user.is_admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "admin access required"})),
        )
            .into_response());
    }
    Ok(())
}

/// GET /v1/admin/retention
pub async fn get_retention(
    State(state): State<Arc<RetentionState>>,
    request: axum::extract::Request,
) -> Result<Response, Response> {
    let session_user = request
        .extensions()
        .get::<SessionUser>()
        .cloned()
        .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
    require_admin(&session_user)?;

    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let default_days = state.default_raw_events_days;

    let result = conn
        .interact(move |conn| {
            // Read global settings
            let raw_events_days: u64 = conn
                .query_row(
                    "SELECT value FROM retention_settings WHERE key = 'raw_events_days'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default_days);

            let hourly_counts_days: u64 = conn
                .query_row(
                    "SELECT value FROM retention_settings WHERE key = 'hourly_counts_days'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(90);

            // Read per-project settings
            let mut stmt = conn
                .prepare(
                    "SELECT pr.project_id, COALESCE(p.name, pr.project_id), pr.raw_events_days
                     FROM project_retention pr
                     LEFT JOIN projects p ON p.id = pr.project_id
                     ORDER BY pr.project_id",
                )
                .unwrap();
            let per_project: Vec<ProjectRetention> = stmt
                .query_map([], |row| {
                    Ok(ProjectRetention {
                        project_id: row.get(0)?,
                        project_name: row.get(1)?,
                        raw_events_days: row.get::<_, i64>(2)? as u64,
                    })
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();

            // Storage stats
            let db_size_bytes: i64 = conn
                .query_row(
                    "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            let raw_events_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
                .unwrap_or(0);

            let hourly_counts_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM event_counts_hourly", [], |row| {
                    row.get(0)
                })
                .unwrap_or(0);

            Ok::<_, rusqlite::Error>(RetentionResponse {
                global: GlobalRetention {
                    raw_events_days,
                    hourly_counts_days,
                },
                per_project,
                storage: StorageStats {
                    db_size_bytes,
                    raw_events_count,
                    hourly_counts_count,
                },
            })
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "interact error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?
        .map_err(|e| {
            tracing::error!(error = %e, "query error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    Ok(Json(result).into_response())
}

/// PUT /v1/admin/retention
pub async fn update_retention(
    State(state): State<Arc<RetentionState>>,
    request: axum::extract::Request,
) -> Result<Response, Response> {
    let session_user = request
        .extensions()
        .get::<SessionUser>()
        .cloned()
        .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
    require_admin(&session_user)?;

    let body_bytes = axum::body::to_bytes(request.into_body(), 4096)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST.into_response())?;
    let input: UpdateRetentionRequest = serde_json::from_slice(&body_bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("invalid request: {e}")})),
        )
            .into_response()
    })?;

    // Validate
    if let Some(days) = input.raw_events_days {
        if days == 0 || days > 3650 {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "raw_events_days must be between 1 and 3650"})),
            )
                .into_response());
        }
    }
    if let Some(days) = input.hourly_counts_days {
        if days == 0 || days > 3650 {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "hourly_counts_days must be between 1 and 3650"})),
            )
                .into_response());
        }
    }

    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    conn.interact(move |conn| {
        let now = chrono::Utc::now().timestamp();

        if let Some(days) = input.raw_events_days {
            conn.execute(
                "INSERT INTO retention_settings (key, value, updated_at)
                 VALUES ('raw_events_days', ?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = ?1, updated_at = ?2",
                params![days.to_string(), now],
            )?;
        }
        if let Some(days) = input.hourly_counts_days {
            conn.execute(
                "INSERT INTO retention_settings (key, value, updated_at)
                 VALUES ('hourly_counts_days', ?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = ?1, updated_at = ?2",
                params![days.to_string(), now],
            )?;
        }

        for pp in &input.per_project {
            if pp.raw_events_days == 0 {
                // Delete override to fall back to global
                conn.execute(
                    "DELETE FROM project_retention WHERE project_id = ?1",
                    params![pp.project_id],
                )?;
            } else {
                conn.execute(
                    "INSERT INTO project_retention (project_id, raw_events_days, updated_at)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT (project_id) DO UPDATE SET raw_events_days = ?2, updated_at = ?3",
                    params![pp.project_id, pp.raw_events_days as i64, now],
                )?;
            }
        }

        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "interact error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?
    .map_err(|e| {
        tracing::error!(error = %e, "db error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    tracing::info!(admin = %session_user.user_id, "retention settings updated");
    Ok(Json(serde_json::json!({"status": "ok"})).into_response())
}

/// POST /v1/admin/retention/purge
pub async fn purge_now(
    State(state): State<Arc<RetentionState>>,
    request: axum::extract::Request,
) -> Result<Response, Response> {
    let session_user = request
        .extensions()
        .get::<SessionUser>()
        .cloned()
        .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
    require_admin(&session_user)?;

    let body_bytes = axum::body::to_bytes(request.into_body(), 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST.into_response())?;
    let input: PurgeRequest = serde_json::from_slice(&body_bytes).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "request body must contain {\"confirm\": true}"})),
        )
            .into_response()
    })?;

    if !input.confirm {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "confirm must be true"})),
        )
            .into_response());
    }

    let pool = state.pool.clone();
    let default_days = state.default_raw_events_days;

    let result = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async { super::retention::run_retention_once(&pool, default_days).await })
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "purge task failed");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    match result {
        Ok((raw_deleted, hourly_deleted)) => {
            tracing::info!(
                raw_deleted,
                hourly_deleted,
                admin = %session_user.user_id,
                "manual purge completed"
            );
            Ok(Json(serde_json::json!({
                "status": "ok",
                "raw_events_deleted": raw_deleted,
                "hourly_counts_deleted": hourly_deleted,
            }))
            .into_response())
        }
        Err(e) => {
            tracing::error!(error = %e, "purge failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}
