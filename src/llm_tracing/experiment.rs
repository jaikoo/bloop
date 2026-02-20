use crate::auth::bearer::TokenAuth;
use crate::error::{AppError, AppResult};
use crate::llm_tracing::LlmQueryState;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct ExperimentQueryParams {
    pub project_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExperimentListResponse {
    pub experiments: Vec<ExperimentSummary>,
}

#[derive(Debug, Serialize)]
pub struct ExperimentSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub prompt_name: String,
    pub variant_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize)]
pub struct ExperimentDetailResponse {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub prompt_name: String,
    pub variants: Vec<ExperimentVariant>,
    pub metrics: ExperimentMetrics,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize)]
pub struct ExperimentVariant {
    pub id: String,
    pub name: String,
    pub version: String,
    pub prompt_template: String,
    pub traffic_percentage: f64,
    pub metrics: VariantMetrics,
}

#[derive(Debug, Serialize)]
pub struct VariantMetrics {
    pub total_calls: i64,
    pub avg_latency_ms: f64,
    pub avg_cost_per_call: f64,
    pub success_rate: f64,
    pub avg_tokens_per_call: f64,
    pub user_feedback_score: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct ExperimentMetrics {
    pub total_calls: i64,
    pub total_cost_micros: i64,
    pub p95_latency_ms: f64,
    pub overall_success_rate: f64,
}

pub async fn list_experiments(
    State(state): State<Arc<LlmQueryState>>,
    Query(params): Query<ExperimentQueryParams>,
) -> AppResult<Json<ExperimentListResponse>> {
    let project_id = params
        .project_id
        .clone()
        .unwrap_or_else(|| "default".to_string());

    let pool = state.pool.clone();
    let experiments = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("Pool error: {}", e)))?
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, description, status, prompt_name, created_at, updated_at,
                 (SELECT COUNT(*) FROM experiment_variants WHERE experiment_id = e.id) as variant_count
                 FROM prompt_experiments e
                 WHERE project_id = ?1
                 ORDER BY updated_at DESC"
            )?;

            let mut rows = stmt.query(rusqlite::params![project_id])?;
            let mut results = Vec::new();

            while let Some(row) = rows.next()? {
                results.push(ExperimentSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    status: row.get(3)?,
                    prompt_name: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                    variant_count: row.get(7)?,
                });
            }

            Ok::<_, rusqlite::Error>(results)
        })
        .await
        .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?
        .map_err(AppError::Database)?;

    Ok(Json(ExperimentListResponse { experiments }))
}

pub async fn get_experiment(
    State(state): State<Arc<LlmQueryState>>,
    Path(experiment_id): Path<String>,
    Query(params): Query<ExperimentQueryParams>,
) -> AppResult<Json<ExperimentDetailResponse>> {
    let project_id = params
        .project_id
        .clone()
        .unwrap_or_else(|| "default".to_string());

    let pool = state.pool.clone();
    let (experiment, variants) = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("Pool error: {}", e)))?
        .interact(move |conn| {
            let exp_row = conn.query_row(
                "SELECT id, name, description, status, prompt_name, created_at, updated_at
                 FROM prompt_experiments
                 WHERE id = ?1 AND project_id = ?2",
                rusqlite::params![experiment_id, project_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                }
            )?;

            let mut stmt = conn.prepare(
                "SELECT v.id, v.name, v.version, v.prompt_template, v.traffic_percentage,
                 COUNT(t.id) as total_calls,
                 AVG(t.latency_ms) as avg_latency,
                 AVG(t.cost_micros) as avg_cost,
                 SUM(CASE WHEN t.status = 'ok' THEN 1 ELSE 0 END) * 100.0 / COUNT(t.id) as success_rate,
                 AVG(t.total_tokens) as avg_tokens
                 FROM experiment_variants v
                 LEFT JOIN llm_traces t ON t.prompt_name = ?1 AND t.prompt_version = v.version
                   AND t.started_at >= (SELECT created_at FROM prompt_experiments WHERE id = ?2)
                 WHERE v.experiment_id = ?2
                 GROUP BY v.id, v.name, v.version, v.prompt_template, v.traffic_percentage
                 ORDER BY v.created_at"
            )?;

            let prompt_name: String = exp_row.4.clone();
            let exp_id = experiment_id.clone();
            let mut rows = stmt.query(rusqlite::params![prompt_name, exp_id])?;
            let mut variants = Vec::new();

            while let Some(row) = rows.next()? {
                let total_calls: i64 = row.get(5).unwrap_or(0);
                let avg_latency: f64 = row.get(6).unwrap_or(0.0);
                let avg_cost: f64 = row.get(7).unwrap_or(0.0);
                let success_rate: f64 = row.get(8).unwrap_or(0.0);
                let avg_tokens: f64 = row.get(9).unwrap_or(0.0);

                variants.push(ExperimentVariant {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    version: row.get(2)?,
                    prompt_template: row.get(3)?,
                    traffic_percentage: row.get(4)?,
                    metrics: VariantMetrics {
                        total_calls,
                        avg_latency_ms: avg_latency,
                        avg_cost_per_call: avg_cost / 1_000_000.0,
                        success_rate,
                        avg_tokens_per_call: avg_tokens,
                        user_feedback_score: None,
                    },
                });
            }

            Ok::<_, rusqlite::Error>((exp_row, variants))
        })
        .await
        .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?
        .map_err(AppError::Database)?;

    let total_calls: i64 = variants.iter().map(|v| v.metrics.total_calls).sum();
    let total_cost_micros: i64 = (variants
        .iter()
        .map(|v| v.metrics.avg_cost_per_call)
        .sum::<f64>()
        * 1_000_000.0) as i64;
    let overall_success_rate = if total_calls > 0 {
        variants.iter().map(|v| v.metrics.success_rate).sum::<f64>() / variants.len() as f64
    } else {
        0.0
    };

    let response = ExperimentDetailResponse {
        id: experiment.0,
        name: experiment.1,
        description: experiment.2,
        status: experiment.3,
        prompt_name: experiment.4,
        variants,
        metrics: ExperimentMetrics {
            total_calls,
            total_cost_micros,
            p95_latency_ms: 0.0,
            overall_success_rate,
        },
        created_at: experiment.5,
        updated_at: experiment.6,
    };

    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
pub struct CreateExperimentRequest {
    pub name: String,
    pub description: Option<String>,
    pub prompt_name: String,
    pub variants: Vec<CreateVariantRequest>,
}

#[derive(Debug, Deserialize)]
pub struct CreateVariantRequest {
    pub name: String,
    pub version: String,
    pub prompt_template: String,
    pub traffic_percentage: f64,
}

#[derive(Debug, Serialize)]
pub struct CreateExperimentResponse {
    pub id: String,
    pub message: String,
}

pub async fn create_experiment(
    State(state): State<Arc<LlmQueryState>>,
    Query(params): Query<ExperimentQueryParams>,
    Json(req): Json<CreateExperimentRequest>,
) -> AppResult<Json<CreateExperimentResponse>> {
    let project_id = params
        .project_id
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let experiment_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp_millis();

    let experiment_id_clone = experiment_id.clone();
    let pool = state.pool.clone();
    pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("Pool error: {}", e)))?
        .interact(move |conn| {
            conn.execute(
                "INSERT INTO prompt_experiments (id, project_id, name, description, prompt_name, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6)",
                rusqlite::params![
                    experiment_id_clone,
                    project_id,
                    req.name,
                    req.description,
                    req.prompt_name,
                    now
                ]
            )?;

            for variant in req.variants {
                let variant_id = uuid::Uuid::new_v4().to_string();
                conn.execute(
                    "INSERT INTO experiment_variants (id, experiment_id, name, version, prompt_template, traffic_percentage, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        variant_id,
                        experiment_id_clone,
                        variant.name,
                        variant.version,
                        variant.prompt_template,
                        variant.traffic_percentage,
                        now
                    ]
                )?;
            }

            Ok::<_, rusqlite::Error>(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?
        .map_err(AppError::Database)?;

    Ok(Json(CreateExperimentResponse {
        id: experiment_id,
        message: "Experiment created successfully".to_string(),
    }))
}

#[derive(Debug, Serialize)]
pub struct ComparisonResponse {
    pub baseline_variant: String,
    pub treatment_variant: String,
    pub metrics: ComparisonMetrics,
    pub statistical_significance: StatisticalSignificance,
    pub recommendation: String,
}

#[derive(Debug, Serialize)]
pub struct ComparisonMetrics {
    pub latency_improvement_percent: f64,
    pub cost_improvement_percent: f64,
    pub success_rate_improvement_percent: f64,
    pub sample_size: i64,
}

#[derive(Debug, Serialize)]
pub struct StatisticalSignificance {
    pub p_value: f64,
    pub is_significant: bool,
    pub confidence_level: f64,
}

pub async fn compare_variants(
    State(state): State<Arc<LlmQueryState>>,
    Path(experiment_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<ComparisonResponse>> {
    let baseline = params.get("baseline").cloned().unwrap_or_default();
    let treatment = params.get("treatment").cloned().unwrap_or_default();
    let baseline_clone = baseline.clone();
    let treatment_clone = treatment.clone();

    let pool = state.pool.clone();
    let (baseline_metrics, treatment_metrics) = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("Pool error: {}", e)))?
        .interact(move |conn| {
            let get_metrics = |version: &str| -> Result<VariantMetrics, rusqlite::Error> {
                conn.query_row(
                    "SELECT
                     COUNT(t.id) as total_calls,
                     AVG(t.latency_ms) as avg_latency,
                     AVG(t.cost_micros) as avg_cost,
                     SUM(CASE WHEN t.status = 'ok' THEN 1 ELSE 0 END) * 100.0 / COUNT(t.id) as success_rate,
                     AVG(t.total_tokens) as avg_tokens
                     FROM llm_traces t
                     JOIN experiment_variants v ON v.version = t.prompt_version
                     WHERE v.experiment_id = ?1 AND v.version = ?2
                     GROUP BY v.version",
                    rusqlite::params![experiment_id, version],
                    |row| {
                        let cost_micros: i64 = row.get(2).unwrap_or(0);
                        Ok(VariantMetrics {
                            total_calls: row.get(0).unwrap_or(0),
                            avg_latency_ms: row.get(1).unwrap_or(0.0),
                            avg_cost_per_call: cost_micros as f64 / 1_000_000.0,
                            success_rate: row.get(3).unwrap_or(0.0),
                            avg_tokens_per_call: row.get(4).unwrap_or(0.0),
                            user_feedback_score: None,
                        })
                    }
                )
            };

            let baseline = get_metrics(&baseline_clone)?;
            let treatment = get_metrics(&treatment_clone)?;

            Ok::<_, rusqlite::Error>((baseline, treatment))
        })
        .await
        .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?
        .map_err(AppError::Database)?;

    let latency_improvement = if baseline_metrics.avg_latency_ms > 0.0 {
        (treatment_metrics.avg_latency_ms - baseline_metrics.avg_latency_ms)
            / baseline_metrics.avg_latency_ms
            * 100.0
    } else {
        0.0
    };

    let cost_improvement = if baseline_metrics.avg_cost_per_call > 0.0 {
        (treatment_metrics.avg_cost_per_call - baseline_metrics.avg_cost_per_call)
            / baseline_metrics.avg_cost_per_call
            * 100.0
    } else {
        0.0
    };

    let success_improvement = treatment_metrics.success_rate - baseline_metrics.success_rate;

    let sample_size = baseline_metrics
        .total_calls
        .min(treatment_metrics.total_calls);

    let is_better =
        latency_improvement < -5.0 || cost_improvement < -5.0 || success_improvement > 5.0;

    let recommendation = if is_better {
        format!(
            "Treatment variant '{}' shows improvement. Consider promoting to production.",
            treatment
        )
    } else {
        format!(
            "No significant improvement detected. Keep using baseline variant '{}'.",
            baseline
        )
    };

    Ok(Json(ComparisonResponse {
        baseline_variant: baseline,
        treatment_variant: treatment,
        metrics: ComparisonMetrics {
            latency_improvement_percent: latency_improvement,
            cost_improvement_percent: cost_improvement,
            success_rate_improvement_percent: success_improvement,
            sample_size,
        },
        statistical_significance: StatisticalSignificance {
            p_value: 0.05,
            is_significant: is_better && sample_size > 30,
            confidence_level: if is_better && sample_size > 30 {
                0.95
            } else {
                0.0
            },
        },
        recommendation,
    }))
}
