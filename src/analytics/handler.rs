use crate::analytics::cache::AnalyticsCache;
use crate::analytics::queries;
use crate::analytics::types::*;
use crate::analytics::AnalyticsState;
use crate::auth::bearer::TokenAuth;
use crate::error::{AppError, AppResult};
use crate::query::handler::resolve_project_for_scope;
use axum::extract::{Query, State};
use axum::Json;
use duckdb::params;
use std::sync::Arc;

/// Helper: resolve project_id from token auth or query param, requiring `errors:read` scope.
fn resolve_project(
    token_auth: &Option<axum::Extension<TokenAuth>>,
    query_project_id: Option<String>,
) -> AppResult<Option<String>> {
    resolve_project_for_scope(token_auth, query_project_id, "errors:read")
}

/// Macro to handle cache hit/miss pattern.
macro_rules! cached_or_compute {
    ($state:expr, $endpoint:expr, $params:expr, $compute:expr) => {{
        let hours = $params.hours();
        let limit = $params.limit();
        let key = AnalyticsCache::cache_key(
            $endpoint,
            &$params.project_id,
            hours,
            limit,
            &$params.environment,
        );
        if let Some(cached) = $state.cache.get(&key) {
            let val: serde_json::Value = serde_json::from_str(&cached)
                .map_err(|e| AppError::Analytics(format!("cache deserialize: {e}")))?;
            return Ok(Json(val));
        }
        let result = $compute;
        let json_str = serde_json::to_string(&result)
            .map_err(|e| AppError::Analytics(format!("serialize: {e}")))?;
        $state.cache.insert(key, json_str);
        Ok(Json(serde_json::to_value(&result).map_err(|e| {
            AppError::Analytics(format!("serialize: {e}"))
        })?))
    }};
}

/// GET /v1/analytics/spikes
pub async fn spike_detection(
    State(state): State<Arc<AnalyticsState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<AnalyticsQueryParams>,
) -> AppResult<Json<serde_json::Value>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let limit = qp.limit();
    let threshold = state.config.zscore_threshold;

    cached_or_compute!(state, "spikes", qp, {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let since = now_ms - (hours * 3_600_000);
        let project_id = qp.project_id.clone();
        let env = qp.environment.clone();

        let spikes = state
            .conn
            .query(move |conn| {
                let mut stmt = conn.prepare(queries::SPIKES_SQL)?;
                let rows =
                    stmt.query_map(params![since, project_id, env, threshold, limit], |row| {
                        Ok(SpikeEntry {
                            fingerprint: row.get(0)?,
                            error_type: row.get(1)?,
                            message: row.get(2)?,
                            hour_bucket: row.get(3)?,
                            count: row.get(4)?,
                            mean: row.get(5)?,
                            stddev: row.get(6)?,
                            zscore: row.get(7)?,
                        })
                    })?;
                rows.collect::<Result<Vec<_>, _>>()
            })
            .await
            .map_err(AppError::Analytics)?;

        SpikeResponse {
            spikes,
            threshold,
            window_hours: hours,
        }
    })
}

/// GET /v1/analytics/movers
pub async fn top_movers(
    State(state): State<Arc<AnalyticsState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<AnalyticsQueryParams>,
) -> AppResult<Json<serde_json::Value>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let limit = qp.limit();

    cached_or_compute!(state, "movers", qp, {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let window = hours * 3_600_000;
        let current_start = now_ms - window;
        let prev_start = now_ms - (window * 2);
        let project_id = qp.project_id.clone();
        let env = qp.environment.clone();

        let movers = state
            .conn
            .query(move |conn| {
                let mut stmt = conn.prepare(queries::MOVERS_SQL)?;
                let rows = stmt.query_map(
                    params![current_start, now_ms, prev_start, project_id, env, limit],
                    |row| {
                        Ok(MoverEntry {
                            fingerprint: row.get(0)?,
                            error_type: row.get(1)?,
                            message: row.get(2)?,
                            count_current: row.get(3)?,
                            count_previous: row.get(4)?,
                            delta: row.get(5)?,
                            delta_pct: row.get(6)?,
                        })
                    },
                )?;
                rows.collect::<Result<Vec<_>, _>>()
            })
            .await
            .map_err(AppError::Analytics)?;

        MoversResponse {
            movers,
            window_hours: hours,
        }
    })
}

/// GET /v1/analytics/correlations
pub async fn error_correlation(
    State(state): State<Arc<AnalyticsState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<AnalyticsQueryParams>,
) -> AppResult<Json<serde_json::Value>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let limit = qp.limit();

    cached_or_compute!(state, "correlations", qp, {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let since = now_ms - (hours * 3_600_000);
        let project_id = qp.project_id.clone();
        let env = qp.environment.clone();

        let pairs = state
            .conn
            .query(move |conn| {
                let mut stmt = conn.prepare(queries::CORRELATIONS_SQL)?;
                let rows = stmt.query_map(params![since, project_id, env, limit], |row| {
                    Ok(CorrelationPair {
                        fingerprint_a: row.get(0)?,
                        fingerprint_b: row.get(1)?,
                        message_a: row.get(2)?,
                        message_b: row.get(3)?,
                        correlation: row.get(4)?,
                    })
                })?;
                rows.collect::<Result<Vec<_>, _>>()
            })
            .await
            .map_err(AppError::Analytics)?;

        CorrelationsResponse {
            pairs,
            window_hours: hours,
        }
    })
}

/// GET /v1/analytics/releases
pub async fn release_impact(
    State(state): State<Arc<AnalyticsState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<AnalyticsQueryParams>,
) -> AppResult<Json<serde_json::Value>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let limit = qp.limit();

    cached_or_compute!(state, "releases", qp, {
        let project_id = qp.project_id.clone();

        let releases = state
            .conn
            .query(move |conn| {
                let mut stmt = conn.prepare(queries::RELEASES_SQL)?;
                let rows = stmt.query_map(params![project_id, limit], |row| {
                    Ok(ReleaseEntry {
                        release: row.get(0)?,
                        first_seen: row.get(1)?,
                        error_count: row.get(2)?,
                        unique_fingerprints: row.get(3)?,
                        new_fingerprints: row.get(4)?,
                        error_delta: row.get(5)?,
                        impact_score: row.get(6)?,
                    })
                })?;
                rows.collect::<Result<Vec<_>, _>>()
            })
            .await
            .map_err(AppError::Analytics)?;

        ReleasesResponse { releases }
    })
}

/// GET /v1/analytics/environments
pub async fn env_breakdown(
    State(state): State<Arc<AnalyticsState>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(mut qp): Query<AnalyticsQueryParams>,
) -> AppResult<Json<serde_json::Value>> {
    qp.project_id = resolve_project(&token_auth, qp.project_id)?;
    let hours = qp.hours();
    let limit = qp.limit();

    cached_or_compute!(state, "environments", qp, {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let since = now_ms - (hours * 3_600_000);
        let project_id = qp.project_id.clone();
        let env = qp.environment.clone();

        let (environments, total_count) = state
            .conn
            .query(move |conn| {
                let mut stmt = conn.prepare(queries::ENVIRONMENTS_SQL)?;
                let mut total: i64 = 0;
                let rows = stmt.query_map(params![since, project_id, env, limit], |row| {
                    let grand_total: i64 = row.get(7)?;
                    Ok((
                        EnvironmentEntry {
                            environment: row.get(0)?,
                            total_count: row.get(1)?,
                            unique_errors: row.get(2)?,
                            pct_of_total: row.get(3)?,
                            p50_hourly: row.get(4)?,
                            p90_hourly: row.get(5)?,
                            p99_hourly: row.get(6)?,
                        },
                        grand_total,
                    ))
                })?;
                let mut envs = Vec::new();
                for row in rows {
                    let (entry, gt) = row?;
                    total = gt;
                    envs.push(entry);
                }
                Ok((envs, total))
            })
            .await
            .map_err(AppError::Analytics)?;

        EnvironmentsResponse {
            environments,
            total_count,
        }
    })
}
