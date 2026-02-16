use crate::error::{AppError, AppResult};
use crate::llm_tracing::pricing::{ModelPrice, PricingTable};
use axum::extract::{Query, State};
use axum::Json;
use deadpool_sqlite::Pool;
use serde::Deserialize;
use std::sync::Arc;

/// Shared state for pricing endpoints.
pub struct PricingState {
    pub pricing: Arc<PricingTable>,
    pub pool: Pool,
}

#[derive(Debug, Deserialize)]
pub struct PricingQuery {
    pub model: Option<String>,
}

/// GET /v1/llm/pricing?model=gpt-4o
/// Returns pricing info for a specific model, or general pricing stats.
pub async fn get_pricing(
    State(state): State<Arc<PricingState>>,
    Query(query): Query<PricingQuery>,
) -> AppResult<Json<serde_json::Value>> {
    if let Some(model) = query.model {
        match state.pricing.get_pricing_info(&model) {
            Some(price) => Ok(Json(serde_json::json!({
                "model": model,
                "input_cost_per_token": price.input_cost_per_token,
                "output_cost_per_token": price.output_cost_per_token,
                "provider": price.provider,
            }))),
            None => Err(AppError::NotFound(format!(
                "no pricing data for model '{model}'"
            ))),
        }
    } else {
        Ok(Json(serde_json::json!({
            "model_count": state.pricing.model_count(),
        })))
    }
}

#[derive(Debug, Deserialize)]
pub struct PricingOverrideInput {
    pub model: String,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub provider: Option<String>,
    pub project_id: Option<String>,
}

/// PUT /v1/llm/pricing/overrides - Set a custom price override for a model.
pub async fn set_pricing_override(
    State(state): State<Arc<PricingState>>,
    Json(input): Json<PricingOverrideInput>,
) -> AppResult<Json<serde_json::Value>> {
    if input.model.is_empty() {
        return Err(AppError::Validation("model name is required".to_string()));
    }
    if input.input_cost_per_token < 0.0 || input.output_cost_per_token < 0.0 {
        return Err(AppError::Validation(
            "costs must be non-negative".to_string(),
        ));
    }

    let model = input.model.clone();
    let input_cost = input.input_cost_per_token;
    let output_cost = input.output_cost_per_token;
    let provider = input.provider.clone();
    let project_id = input.project_id.clone();
    let now = chrono::Utc::now().timestamp();

    // Persist to SQLite
    let pool = state.pool.clone();
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let m = model.clone();
    let p = provider.clone();
    let pid = project_id.clone();
    conn.interact(move |conn| {
        conn.execute(
            "INSERT INTO llm_pricing_overrides (model, project_id, input_cost_per_token, output_cost_per_token, provider, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (model, COALESCE(project_id, '__global__'))
             DO UPDATE SET input_cost_per_token = ?3, output_cost_per_token = ?4, provider = ?5, updated_at = ?6",
            rusqlite::params![m, pid, input_cost, output_cost, p, now],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
    .map_err(AppError::Database)?;

    // Update in-memory override (global only for now)
    if project_id.is_none() {
        state.pricing.set_override(
            model.clone(),
            ModelPrice {
                input_cost_per_token: input_cost,
                output_cost_per_token: output_cost,
                provider,
            },
        );
    }

    Ok(Json(serde_json::json!({
        "status": "ok",
        "model": model,
    })))
}

/// Load pricing overrides from the database into the PricingTable.
pub async fn load_overrides_from_db(pool: &Pool, pricing: &PricingTable) -> Result<(), String> {
    let conn = pool.get().await.map_err(|e| format!("pool error: {e}"))?;
    let overrides = conn
        .interact(|conn| {
            let mut stmt = conn.prepare(
                "SELECT model, input_cost_per_token, output_cost_per_token, provider
                 FROM llm_pricing_overrides
                 WHERE project_id IS NULL",
            )?;
            let rows: Vec<(String, ModelPrice)> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        ModelPrice {
                            input_cost_per_token: row.get(1)?,
                            output_cost_per_token: row.get(2)?,
                            provider: row.get(3)?,
                        },
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| format!("interact error: {e}"))?
        .map_err(|e| format!("db error: {e}"))?;

    if !overrides.is_empty() {
        tracing::info!(
            count = overrides.len(),
            "loaded pricing overrides from database"
        );
    }
    pricing.load_overrides(overrides);
    Ok(())
}
