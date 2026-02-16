use crate::auth::bearer::TokenAuth;
use crate::error::{AppError, AppResult};
use crate::llm_tracing::types::*;
use crate::query::handler::resolve_project_for_scope;
use axum::extract::{Query, State};
use axum::Json;
use chrono::Datelike;
use deadpool_sqlite::Pool;
use rusqlite::params;
use std::sync::Arc;

fn resolve_project(
    token_auth: &Option<axum::Extension<TokenAuth>>,
    query_project_id: Option<String>,
) -> AppResult<Option<String>> {
    resolve_project_for_scope(token_auth, query_project_id, "errors:read")
}

/// Compute the current month's spend from llm_usage_hourly.
fn compute_month_spend(conn: &rusqlite::Connection, project_id: &str) -> i64 {
    let now = chrono::Utc::now();
    let month_start = now.date_naive().with_day(1).unwrap_or(now.date_naive());
    let month_start_ms = month_start
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();

    conn.query_row(
        "SELECT COALESCE(SUM(cost_micros), 0)
         FROM llm_usage_hourly
         WHERE project_id = ?1 AND hour_bucket >= ?2",
        params![project_id, month_start_ms],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// Compute forecast and budget metadata.
fn build_budget_response(
    project_id: String,
    monthly_budget_micros: i64,
    alert_threshold_pct: i64,
    current_month_spend_micros: i64,
) -> BudgetResponse {
    let now = chrono::Utc::now();
    let day_of_month = now.format("%d").to_string().parse::<i64>().unwrap_or(1);
    let days_in_month = {
        let year = now.format("%Y").to_string().parse::<i32>().unwrap_or(2026);
        let month = now.format("%m").to_string().parse::<u32>().unwrap_or(1);
        // Days in current month
        if month == 12 {
            chrono::NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)
        }
        .and_then(|d| d.pred_opt())
        .map(|d| d.day() as i64)
        .unwrap_or(30)
    };

    let days_elapsed = day_of_month;
    let days_remaining = (days_in_month - day_of_month).max(0);

    let forecast_month_end_micros = if days_elapsed > 0 {
        let daily_rate = current_month_spend_micros as f64 / days_elapsed as f64;
        (daily_rate * days_in_month as f64).round() as i64
    } else {
        0
    };

    let budget_used_pct = if monthly_budget_micros > 0 {
        current_month_spend_micros as f64 * 100.0 / monthly_budget_micros as f64
    } else {
        0.0
    };

    let forecast_over_budget =
        monthly_budget_micros > 0 && forecast_month_end_micros > monthly_budget_micros;

    BudgetResponse {
        project_id,
        monthly_budget_micros,
        alert_threshold_pct,
        current_month_spend_micros,
        budget_used_pct,
        forecast_month_end_micros,
        forecast_over_budget,
        days_elapsed,
        days_remaining,
    }
}

/// GET /v1/llm/budget
pub async fn get_budget(
    State(pool): State<Arc<Pool>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(qp): Query<LlmQueryParams>,
) -> AppResult<Json<BudgetResponse>> {
    let project_id =
        resolve_project(&token_auth, qp.project_id)?.unwrap_or_else(|| "default".to_string());

    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let pid = project_id.clone();

    let result = conn
        .interact(move |conn| {
            // Load budget (or defaults if not set)
            let (monthly_budget_micros, alert_threshold_pct): (i64, i64) = conn
                .query_row(
                    "SELECT monthly_budget_micros, alert_threshold_pct
                     FROM llm_cost_budgets WHERE project_id = ?1",
                    params![pid],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap_or((0, 80));

            let current_month_spend_micros = compute_month_spend(conn, &pid);

            Ok::<_, rusqlite::Error>(build_budget_response(
                pid,
                monthly_budget_micros,
                alert_threshold_pct,
                current_month_spend_micros,
            ))
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
        .map_err(AppError::Database)?;

    Ok(Json(result))
}

/// PUT /v1/llm/budget
pub async fn set_budget(
    State(pool): State<Arc<Pool>>,
    token_auth: Option<axum::Extension<TokenAuth>>,
    Query(qp): Query<LlmQueryParams>,
    Json(input): Json<BudgetInput>,
) -> AppResult<Json<BudgetResponse>> {
    let project_id =
        resolve_project(&token_auth, qp.project_id)?.unwrap_or_else(|| "default".to_string());

    if input.monthly_budget_micros < 0 {
        return Err(AppError::Validation(
            "monthly_budget_micros must be >= 0".into(),
        ));
    }

    let alert_threshold_pct = input.alert_threshold_pct.unwrap_or(80).clamp(1, 100);
    let now = chrono::Utc::now().timestamp_millis();

    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let pid = project_id.clone();
    let budget = input.monthly_budget_micros;
    let threshold = alert_threshold_pct;

    let result = conn
        .interact(move |conn| {
            conn.execute(
                "INSERT INTO llm_cost_budgets (project_id, monthly_budget_micros, alert_threshold_pct, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (project_id) DO UPDATE SET
                    monthly_budget_micros = excluded.monthly_budget_micros,
                    alert_threshold_pct = excluded.alert_threshold_pct,
                    updated_at = excluded.updated_at",
                params![pid, budget, threshold, now],
            )?;

            let current_month_spend_micros = compute_month_spend(conn, &pid);

            Ok::<_, rusqlite::Error>(build_budget_response(
                pid,
                budget,
                threshold,
                current_month_spend_micros,
            ))
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
        .map_err(AppError::Database)?;

    Ok(Json(result))
}
