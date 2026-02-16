use crate::auth::bearer::TokenAuth;
use crate::error::{AppError, AppResult};
use crate::pipeline::aggregator::SharedAggregator;
use crate::types::{
    ErrorAggregate, ErrorQueryParams, HealthResponse, HourlyCount, RouteErrorCount,
    SampleOccurrence, StatsResponse, StatusChange, TrendQueryParams,
};
use axum::extract::{Path, Query, State};
use axum::Json;
use deadpool_sqlite::Pool;
use rusqlite::params;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Extract the effective project_id from bearer token (always project-scoped) or query param.
/// Also checks the token has the required scope. Session auth skips scope checks.
pub(crate) fn resolve_project_for_scope(
    token_auth: &Option<axum::Extension<TokenAuth>>,
    query_project_id: Option<String>,
    required_scope: &str,
) -> AppResult<Option<String>> {
    if let Some(axum::Extension(ref auth)) = token_auth {
        if !auth.scopes.iter().any(|s| s == required_scope) {
            return Err(AppError::Auth(format!(
                "token lacks required scope: {required_scope}"
            )));
        }
        Ok(Some(auth.project_id.clone()))
    } else {
        Ok(query_project_id)
    }
}

pub struct QueryState {
    pub pool: Pool,
    #[allow(dead_code)]
    pub aggregator: SharedAggregator,
    pub channel_capacity: usize,
    pub channel_tx: mpsc::Sender<crate::types::ProcessedEvent>,
}

/// GET /v1/errors - List aggregated errors with filters and pagination.
pub async fn list_errors(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut params): Query<ErrorQueryParams>,
) -> AppResult<Json<Vec<ErrorAggregate>>> {
    params.project_id = resolve_project_for_scope(&token_auth, params.project_id, "errors:read")?;
    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let results = conn
        .interact(move |conn| {
            let mut sql = String::from(
                "SELECT project_id, fingerprint, release, environment, total_count, first_seen, last_seen,
                        error_type, message, source, route_or_procedure, screen, status
                 FROM error_aggregates WHERE 1=1",
            );
            let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

            if let Some(ref project_id) = params.project_id {
                sql.push_str(&format!(" AND project_id = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(project_id.clone()));
            }
            if let Some(ref release) = params.release {
                sql.push_str(&format!(" AND release = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(release.clone()));
            }
            if let Some(ref environment) = params.environment {
                sql.push_str(&format!(" AND environment = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(environment.clone()));
            }
            if let Some(ref source) = params.source {
                sql.push_str(&format!(" AND source = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(source.clone()));
            }
            if let Some(ref route) = params.route {
                sql.push_str(&format!(
                    " AND route_or_procedure = ?{}",
                    bind_values.len() + 1
                ));
                bind_values.push(Box::new(route.clone()));
            }
            if let Some(ref status) = params.status {
                sql.push_str(&format!(" AND status = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(status.clone()));
            }
            if let Some(since) = params.since {
                sql.push_str(&format!(" AND last_seen >= ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(since));
            }
            if let Some(until) = params.until {
                sql.push_str(&format!(" AND last_seen <= ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(until));
            }

            // Sort (validated to prevent unexpected values)
            let sort = params.sort.as_deref().unwrap_or("last_seen");
            match sort {
                "total_count" => sql.push_str(" ORDER BY total_count DESC"),
                "first_seen" => sql.push_str(" ORDER BY first_seen DESC"),
                "last_seen" => sql.push_str(" ORDER BY last_seen DESC"),
                _ => {
                    return Err(rusqlite::Error::InvalidParameterName(format!(
                        "invalid sort field: {sort}. Valid values: last_seen, first_seen, total_count"
                    )));
                }
            }

            let limit = params.limit.unwrap_or(50).min(200);
            let offset = params.offset.unwrap_or(0);
            sql.push_str(&format!(
                " LIMIT ?{} OFFSET ?{}",
                bind_values.len() + 1,
                bind_values.len() + 2
            ));
            bind_values.push(Box::new(limit));
            bind_values.push(Box::new(offset));

            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                bind_values.iter().map(|b| b.as_ref()).collect();

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_ref.as_slice(), |row| {
                Ok(ErrorAggregate {
                    project_id: row.get(0)?,
                    fingerprint: row.get(1)?,
                    release: row.get(2)?,
                    environment: row.get(3)?,
                    total_count: row.get(4)?,
                    first_seen: row.get(5)?,
                    last_seen: row.get(6)?,
                    error_type: row.get(7)?,
                    message: row.get(8)?,
                    source: row.get(9)?,
                    route_or_procedure: row.get(10)?,
                    screen: row.get(11)?,
                    status: row.get(12)?,
                })
            })?;

            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok::<_, rusqlite::Error>(results)
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(Json(results))
}

/// GET /v1/errors/:fingerprint - Error detail with sample occurrences.
pub async fn get_error_detail(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(fingerprint): Path<String>,
    Query(params): Query<ProjectIdParam>,
) -> AppResult<Json<serde_json::Value>> {
    let fp = fingerprint.clone();
    let project_id = resolve_project_for_scope(&token_auth, params.project_id, "errors:read")?;
    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let result = conn
        .interact(move |conn| {
            // Get aggregate info (across all releases)
            let (agg_sql, agg_params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
                if let Some(ref pid) = project_id {
                    (
                        "SELECT project_id, fingerprint, release, environment, total_count, first_seen, last_seen,
                                error_type, message, source, route_or_procedure, screen, status
                         FROM error_aggregates WHERE fingerprint = ?1 AND project_id = ?2
                         ORDER BY last_seen DESC".to_string(),
                        vec![Box::new(fp.clone()) as Box<dyn rusqlite::types::ToSql>, Box::new(pid.clone())],
                    )
                } else {
                    (
                        "SELECT project_id, fingerprint, release, environment, total_count, first_seen, last_seen,
                                error_type, message, source, route_or_procedure, screen, status
                         FROM error_aggregates WHERE fingerprint = ?1
                         ORDER BY last_seen DESC".to_string(),
                        vec![Box::new(fp.clone()) as Box<dyn rusqlite::types::ToSql>],
                    )
                };

            let agg_refs: Vec<&dyn rusqlite::types::ToSql> =
                agg_params.iter().map(|b| b.as_ref()).collect();
            let mut stmt = conn.prepare(&agg_sql)?;
            let aggregates: Vec<ErrorAggregate> = stmt
                .query_map(agg_refs.as_slice(), |row| {
                    Ok(ErrorAggregate {
                        project_id: row.get(0)?,
                        fingerprint: row.get(1)?,
                        release: row.get(2)?,
                        environment: row.get(3)?,
                        total_count: row.get(4)?,
                        first_seen: row.get(5)?,
                        last_seen: row.get(6)?,
                        error_type: row.get(7)?,
                        message: row.get(8)?,
                        source: row.get(9)?,
                        route_or_procedure: row.get(10)?,
                        screen: row.get(11)?,
                        status: row.get(12)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            // Get sample occurrences
            let (sample_sql, sample_params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
                if let Some(ref pid) = project_id {
                    (
                        "SELECT id, fingerprint, project_id, captured_at, source, environment, release,
                                error_type, message, stack, request_id, metadata
                         FROM sample_occurrences WHERE fingerprint = ?1 AND project_id = ?2
                         ORDER BY captured_at DESC LIMIT 10".to_string(),
                        vec![Box::new(fp.clone()) as Box<dyn rusqlite::types::ToSql>, Box::new(pid.clone())],
                    )
                } else {
                    (
                        "SELECT id, fingerprint, project_id, captured_at, source, environment, release,
                                error_type, message, stack, request_id, metadata
                         FROM sample_occurrences WHERE fingerprint = ?1
                         ORDER BY captured_at DESC LIMIT 10".to_string(),
                        vec![Box::new(fp.clone()) as Box<dyn rusqlite::types::ToSql>],
                    )
                };

            let sample_refs: Vec<&dyn rusqlite::types::ToSql> =
                sample_params.iter().map(|b| b.as_ref()).collect();
            let mut stmt = conn.prepare(&sample_sql)?;
            let samples: Vec<SampleOccurrence> = stmt
                .query_map(sample_refs.as_slice(), |row| {
                    let metadata_str: Option<String> = row.get(11)?;
                    let metadata = metadata_str
                        .and_then(|s| serde_json::from_str(&s).ok());
                    Ok(SampleOccurrence {
                        id: row.get(0)?,
                        fingerprint: row.get(1)?,
                        project_id: row.get(2)?,
                        captured_at: row.get(3)?,
                        source: row.get(4)?,
                        environment: row.get(5)?,
                        release: row.get(6)?,
                        error_type: row.get(7)?,
                        message: row.get(8)?,
                        stack: row.get(9)?,
                        request_id: row.get(10)?,
                        metadata,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok::<_, rusqlite::Error>((aggregates, samples))
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    let (aggregates, samples) = result;
    if aggregates.is_empty() {
        return Err(AppError::NotFound(format!(
            "no errors with fingerprint {fingerprint}"
        )));
    }

    // Attempt deobfuscation for samples with stacks
    let deobfuscated_samples: Vec<serde_json::Value> = if let Some(agg) = aggregates.first() {
        let pid = agg.project_id.clone();
        let release = agg.release.clone();
        let samps = samples.clone();

        let conn2 = state
            .pool
            .get()
            .await
            .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

        conn2
            .interact(move |conn| {
                samps
                    .iter()
                    .map(|s| {
                        let mut val = serde_json::to_value(s).unwrap_or_default();
                        if let Some(ref stack) = s.stack {
                            let frames =
                                crate::sourcemap::deobfuscate_stack(conn, &pid, &release, stack);
                            if !frames.is_empty()
                                && frames.iter().any(|f| f.original_file.is_some())
                            {
                                val["deobfuscated_stack"] =
                                    serde_json::to_value(&frames).unwrap_or_default();
                            }
                        }
                        val
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_else(|_| {
                samples
                    .iter()
                    .map(|s| serde_json::to_value(s).unwrap_or_default())
                    .collect()
            })
    } else {
        samples
            .iter()
            .map(|s| serde_json::to_value(s).unwrap_or_default())
            .collect()
    };

    Ok(Json(serde_json::json!({
        "aggregates": aggregates,
        "samples": deobfuscated_samples,
    })))
}

/// Optional project_id query param.
#[derive(Debug, serde::Deserialize)]
pub struct ProjectIdParam {
    pub project_id: Option<String>,
}

/// GET /v1/errors/:fingerprint/occurrences - Raw events for a fingerprint.
pub async fn get_occurrences(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(fingerprint): Path<String>,
    Query(params): Query<crate::types::PaginationParams>,
) -> AppResult<Json<Vec<serde_json::Value>>> {
    let project_id = resolve_project_for_scope(&token_auth, None, "errors:read")?;
    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;
    let limit = params.limit();
    let offset = params.offset();

    let results = conn
        .interact(move |conn| {
            let (sql, query_params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
                if let Some(ref pid) = project_id {
                    (
                        "SELECT id, timestamp, source, environment, release, error_type, message,
                                stack, http_status, request_id, route_or_procedure, fingerprint, received_at, project_id
                         FROM raw_events WHERE fingerprint = ?1 AND project_id = ?2
                         ORDER BY timestamp DESC LIMIT ?3 OFFSET ?4".to_string(),
                        vec![
                            Box::new(fingerprint) as Box<dyn rusqlite::types::ToSql>,
                            Box::new(pid.clone()),
                            Box::new(limit),
                            Box::new(offset),
                        ],
                    )
                } else {
                    (
                        "SELECT id, timestamp, source, environment, release, error_type, message,
                                stack, http_status, request_id, route_or_procedure, fingerprint, received_at, project_id
                         FROM raw_events WHERE fingerprint = ?1
                         ORDER BY timestamp DESC LIMIT ?2 OFFSET ?3".to_string(),
                        vec![
                            Box::new(fingerprint) as Box<dyn rusqlite::types::ToSql>,
                            Box::new(limit),
                            Box::new(offset),
                        ],
                    )
                };
            let refs: Vec<&dyn rusqlite::types::ToSql> = query_params.iter().map(|b| b.as_ref()).collect();
            let mut stmt = conn.prepare(&sql)?;
            let rows: Vec<serde_json::Value> = stmt
                .query_map(refs.as_slice(), |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, i64>(0)?,
                        "timestamp": row.get::<_, i64>(1)?,
                        "source": row.get::<_, String>(2)?,
                        "environment": row.get::<_, String>(3)?,
                        "release": row.get::<_, String>(4)?,
                        "error_type": row.get::<_, String>(5)?,
                        "message": row.get::<_, String>(6)?,
                        "stack": row.get::<_, Option<String>>(7)?,
                        "http_status": row.get::<_, Option<i64>>(8)?,
                        "request_id": row.get::<_, Option<String>>(9)?,
                        "route_or_procedure": row.get::<_, Option<String>>(10)?,
                        "fingerprint": row.get::<_, String>(11)?,
                        "received_at": row.get::<_, i64>(12)?,
                        "project_id": row.get::<_, Option<String>>(13)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(Json(results))
}

/// GET /v1/releases/:release/errors - Top errors for a release.
pub async fn release_errors(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(release): Path<String>,
    Query(params): Query<crate::types::PaginationParams>,
) -> AppResult<Json<Vec<ErrorAggregate>>> {
    let project_id = resolve_project_for_scope(&token_auth, None, "errors:read")?;
    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;
    let limit = params.limit();
    let offset = params.offset();

    let results = conn
        .interact(move |conn| {
            let (sql, query_params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
                if let Some(ref pid) = project_id {
                    (
                        "SELECT project_id, fingerprint, release, environment, total_count, first_seen, last_seen,
                                error_type, message, source, route_or_procedure, screen, status
                         FROM error_aggregates WHERE release = ?1 AND project_id = ?2
                         ORDER BY total_count DESC LIMIT ?3 OFFSET ?4".to_string(),
                        vec![
                            Box::new(release.clone()) as Box<dyn rusqlite::types::ToSql>,
                            Box::new(pid.clone()),
                            Box::new(limit),
                            Box::new(offset),
                        ],
                    )
                } else {
                    (
                        "SELECT project_id, fingerprint, release, environment, total_count, first_seen, last_seen,
                                error_type, message, source, route_or_procedure, screen, status
                         FROM error_aggregates WHERE release = ?1
                         ORDER BY total_count DESC LIMIT ?2 OFFSET ?3".to_string(),
                        vec![
                            Box::new(release.clone()) as Box<dyn rusqlite::types::ToSql>,
                            Box::new(limit),
                            Box::new(offset),
                        ],
                    )
                };

            let refs: Vec<&dyn rusqlite::types::ToSql> = query_params.iter().map(|b| b.as_ref()).collect();
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(refs.as_slice(), |row| {
                    Ok(ErrorAggregate {
                        project_id: row.get(0)?,
                        fingerprint: row.get(1)?,
                        release: row.get(2)?,
                        environment: row.get(3)?,
                        total_count: row.get(4)?,
                        first_seen: row.get(5)?,
                        last_seen: row.get(6)?,
                        error_type: row.get(7)?,
                        message: row.get(8)?,
                        source: row.get(9)?,
                        route_or_procedure: row.get(10)?,
                        screen: row.get(11)?,
                        status: row.get(12)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(Json(results))
}

/// GET /v1/stats - Overview statistics.
pub async fn stats(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(params): Query<ProjectIdParam>,
) -> AppResult<Json<StatsResponse>> {
    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let project_id = resolve_project_for_scope(&token_auth, params.project_id, "errors:read")?;

    let result = conn
        .interact(move |conn| {
            let (total_errors, unresolved_errors) = if let Some(ref pid) = project_id {
                let total: i64 = conn.query_row(
                    "SELECT COUNT(DISTINCT fingerprint) FROM error_aggregates WHERE project_id = ?1",
                    params![pid],
                    |row| row.get(0),
                )?;
                let unresolved: i64 = conn.query_row(
                    "SELECT COUNT(DISTINCT fingerprint) FROM error_aggregates WHERE status = 'unresolved' AND project_id = ?1",
                    params![pid],
                    |row| row.get(0),
                )?;
                (total, unresolved)
            } else {
                let total: i64 = conn.query_row(
                    "SELECT COUNT(DISTINCT fingerprint) FROM error_aggregates",
                    [],
                    |row| row.get(0),
                )?;
                let unresolved: i64 = conn.query_row(
                    "SELECT COUNT(DISTINCT fingerprint) FROM error_aggregates WHERE status = 'unresolved'",
                    [],
                    |row| row.get(0),
                )?;
                (total, unresolved)
            };

            let now_ms = chrono::Utc::now().timestamp_millis();
            let day_ago = now_ms - 86_400_000;
            let total_events_24h: i64 = if let Some(ref pid) = project_id {
                conn.query_row(
                    "SELECT COUNT(*) FROM raw_events WHERE timestamp >= ?1 AND project_id = ?2",
                    params![day_ago, pid],
                    |row| row.get(0),
                )?
            } else {
                conn.query_row(
                    "SELECT COUNT(*) FROM raw_events WHERE timestamp >= ?1",
                    params![day_ago],
                    |row| row.get(0),
                )?
            };

            let top_routes = if let Some(ref pid) = project_id {
                let mut stmt = conn.prepare(
                    "SELECT route_or_procedure, SUM(total_count) as cnt
                     FROM error_aggregates
                     WHERE route_or_procedure IS NOT NULL AND project_id = ?1
                     GROUP BY route_or_procedure
                     ORDER BY cnt DESC LIMIT 10",
                )?;
                let rows = stmt.query_map(params![pid], |row| {
                    Ok(RouteErrorCount {
                        route: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
                rows
            } else {
                let mut stmt = conn.prepare(
                    "SELECT route_or_procedure, SUM(total_count) as cnt
                     FROM error_aggregates
                     WHERE route_or_procedure IS NOT NULL
                     GROUP BY route_or_procedure
                     ORDER BY cnt DESC LIMIT 10",
                )?;
                let rows = stmt.query_map([], |row| {
                    Ok(RouteErrorCount {
                        route: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
                rows
            };

            Ok::<_, rusqlite::Error>(StatsResponse {
                total_errors,
                unresolved_errors,
                total_events_24h,
                top_routes,
            })
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(Json(result))
}

/// GET /health - Health check.
pub async fn health(State(state): State<Arc<QueryState>>) -> Json<HealthResponse> {
    let db_ok = match state.pool.get().await {
        Ok(conn) => conn
            .interact(|conn| conn.execute_batch("SELECT 1"))
            .await
            .is_ok(),
        Err(_) => false,
    };

    // Approximate buffer usage from channel capacity
    let buffer_usage = 1.0 - (state.channel_tx.capacity() as f64 / state.channel_capacity as f64);

    Json(HealthResponse {
        status: if db_ok {
            "ok".into()
        } else {
            "degraded".into()
        },
        db_ok,
        buffer_usage,
    })
}

/// POST /v1/errors/:fingerprint/resolve - Mark error as resolved.
pub async fn resolve_error(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(fingerprint): Path<String>,
    Query(params): Query<ProjectIdParam>,
) -> AppResult<Json<serde_json::Value>> {
    let pid = resolve_project_for_scope(&token_auth, params.project_id, "errors:write")?;
    update_status(&state.pool, &fingerprint, "resolved", pid.as_deref()).await
}

/// POST /v1/errors/:fingerprint/ignore - Mark error as ignored.
pub async fn ignore_error(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(fingerprint): Path<String>,
    Query(params): Query<ProjectIdParam>,
) -> AppResult<Json<serde_json::Value>> {
    let pid = resolve_project_for_scope(&token_auth, params.project_id, "errors:write")?;
    update_status(&state.pool, &fingerprint, "ignored", pid.as_deref()).await
}

/// POST /v1/errors/:fingerprint/mute - Mute error.
pub async fn mute_error(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(fingerprint): Path<String>,
    Query(params): Query<ProjectIdParam>,
) -> AppResult<Json<serde_json::Value>> {
    let pid = resolve_project_for_scope(&token_auth, params.project_id, "errors:write")?;
    update_status(&state.pool, &fingerprint, "muted", pid.as_deref()).await
}

/// POST /v1/errors/:fingerprint/unresolve - Unresolve error.
pub async fn unresolve_error(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(fingerprint): Path<String>,
    Query(params): Query<ProjectIdParam>,
) -> AppResult<Json<serde_json::Value>> {
    let pid = resolve_project_for_scope(&token_auth, params.project_id, "errors:write")?;
    update_status(&state.pool, &fingerprint, "unresolved", pid.as_deref()).await
}

async fn update_status(
    pool: &Pool,
    fingerprint: &str,
    new_status: &str,
    project_id: Option<&str>,
) -> AppResult<Json<serde_json::Value>> {
    // Require project_id to prevent cross-project status mutations
    let Some(project_id) = project_id else {
        return Err(AppError::Validation("project_id is required".to_string()));
    };

    let fp = fingerprint.to_string();
    let st = new_status.to_string();
    let pid = Some(project_id.to_string());

    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let updated = conn
        .interact(move |conn| {
            // Read old status for audit trail
            let old_status: Option<String> = if let Some(ref pid) = pid {
                conn.query_row(
                    "SELECT status FROM error_aggregates WHERE fingerprint = ?1 AND project_id = ?2 LIMIT 1",
                    params![fp, pid],
                    |row| row.get(0),
                ).ok()
            } else {
                conn.query_row(
                    "SELECT status FROM error_aggregates WHERE fingerprint = ?1 LIMIT 1",
                    params![fp],
                    |row| row.get(0),
                ).ok()
            };

            let count = if let Some(ref pid) = pid {
                conn.execute(
                    "UPDATE error_aggregates SET status = ?1 WHERE fingerprint = ?2 AND project_id = ?3",
                    params![st, fp, pid],
                )?
            } else {
                conn.execute(
                    "UPDATE error_aggregates SET status = ?1 WHERE fingerprint = ?2",
                    params![st, fp],
                )?
            };

            // Record status change in audit trail
            if count > 0 {
                let old = old_status.unwrap_or_default();
                let now_ms = chrono::Utc::now().timestamp_millis();
                let project = pid.as_deref().unwrap_or("default");
                conn.execute(
                    "INSERT INTO status_changes (project_id, fingerprint, old_status, new_status, changed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![project, fp, old, st, now_ms],
                )?;
            }

            Ok::<_, rusqlite::Error>(count)
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    if updated == 0 {
        return Err(AppError::NotFound(format!(
            "no errors with fingerprint {fingerprint}"
        )));
    }

    Ok(Json(serde_json::json!({
        "fingerprint": fingerprint,
        "status": new_status,
        "updated": updated,
    })))
}

/// GET /v1/trends - Global hourly event counts.
pub async fn global_trends(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(params): Query<TrendQueryParams>,
) -> AppResult<Json<Vec<HourlyCount>>> {
    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let hours = params.hours.unwrap_or(72).min(720); // Cap at 30 days
    let project_id = resolve_project_for_scope(&token_auth, params.project_id, "errors:read")?;

    let results = conn
        .interact(move |conn| {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let since = now_ms - (hours * 3_600_000);

            if let Some(ref pid) = project_id {
                let mut stmt = conn.prepare(
                    "SELECT hour_bucket, SUM(count) as total
                     FROM event_counts_hourly
                     WHERE project_id = ?1 AND hour_bucket >= ?2
                     GROUP BY hour_bucket
                     ORDER BY hour_bucket ASC",
                )?;
                let rows = stmt
                    .query_map(params![pid, since], |row| {
                        Ok(HourlyCount {
                            hour_bucket: row.get(0)?,
                            count: row.get(1)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, rusqlite::Error>(rows)
            } else {
                let mut stmt = conn.prepare(
                    "SELECT hour_bucket, SUM(count) as total
                     FROM event_counts_hourly
                     WHERE hour_bucket >= ?1
                     GROUP BY hour_bucket
                     ORDER BY hour_bucket ASC",
                )?;
                let rows = stmt
                    .query_map(params![since], |row| {
                        Ok(HourlyCount {
                            hour_bucket: row.get(0)?,
                            count: row.get(1)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, rusqlite::Error>(rows)
            }
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(Json(results))
}

/// GET /v1/errors/:fingerprint/trend - Per-error hourly counts.
pub async fn error_trend(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(fingerprint): Path<String>,
    Query(params): Query<TrendQueryParams>,
) -> AppResult<Json<Vec<HourlyCount>>> {
    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let hours = params.hours.unwrap_or(72).min(720); // Cap at 30 days
    let project_id = resolve_project_for_scope(&token_auth, params.project_id, "errors:read")?;

    let results = conn
        .interact(move |conn| {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let since = now_ms - (hours * 3_600_000);

            if let Some(ref pid) = project_id {
                let mut stmt = conn.prepare(
                    "SELECT hour_bucket, SUM(count) as total
                     FROM event_counts_hourly
                     WHERE fingerprint = ?1 AND project_id = ?2 AND hour_bucket >= ?3
                     GROUP BY hour_bucket
                     ORDER BY hour_bucket ASC",
                )?;
                let rows = stmt
                    .query_map(params![fingerprint, pid, since], |row| {
                        Ok(HourlyCount {
                            hour_bucket: row.get(0)?,
                            count: row.get(1)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, rusqlite::Error>(rows)
            } else {
                let mut stmt = conn.prepare(
                    "SELECT hour_bucket, SUM(count) as total
                     FROM event_counts_hourly
                     WHERE fingerprint = ?1 AND hour_bucket >= ?2
                     GROUP BY hour_bucket
                     ORDER BY hour_bucket ASC",
                )?;
                let rows = stmt
                    .query_map(params![fingerprint, since], |row| {
                        Ok(HourlyCount {
                            hour_bucket: row.get(0)?,
                            count: row.get(1)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, rusqlite::Error>(rows)
            }
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(Json(results))
}

/// GET /v1/errors/:fingerprint/history - Status change audit trail.
pub async fn error_history(
    State(state): State<Arc<QueryState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Path(fingerprint): Path<String>,
    Query(params): Query<ProjectIdParam>,
) -> AppResult<Json<Vec<StatusChange>>> {
    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let project_id = resolve_project_for_scope(&token_auth, params.project_id, "errors:read")?;

    let results = conn
        .interact(move |conn| {
            if let Some(ref pid) = project_id {
                let mut stmt = conn.prepare(
                    "SELECT id, project_id, fingerprint, old_status, new_status, changed_by, changed_at
                     FROM status_changes
                     WHERE fingerprint = ?1 AND project_id = ?2
                     ORDER BY changed_at DESC",
                )?;
                let rows = stmt
                    .query_map(params![fingerprint, pid], |row| {
                        Ok(StatusChange {
                            id: row.get(0)?,
                            project_id: row.get(1)?,
                            fingerprint: row.get(2)?,
                            old_status: row.get(3)?,
                            new_status: row.get(4)?,
                            changed_by: row.get(5)?,
                            changed_at: row.get(6)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, rusqlite::Error>(rows)
            } else {
                let mut stmt = conn.prepare(
                    "SELECT id, project_id, fingerprint, old_status, new_status, changed_by, changed_at
                     FROM status_changes
                     WHERE fingerprint = ?1
                     ORDER BY changed_at DESC",
                )?;
                let rows = stmt
                    .query_map(params![fingerprint], |row| {
                        Ok(StatusChange {
                            id: row.get(0)?,
                            project_id: row.get(1)?,
                            fingerprint: row.get(2)?,
                            old_status: row.get(3)?,
                            new_status: row.get(4)?,
                            changed_by: row.get(5)?,
                            changed_at: row.get(6)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, rusqlite::Error>(rows)
            }
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(Json(results))
}
