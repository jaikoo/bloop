use crate::alert::dispatch::AlertDispatcher;
use crate::alert::rules::load_channels_for_rule;
use chrono::Datelike;
use deadpool_sqlite::Pool;
use rusqlite::params;
use std::sync::Arc;
use std::time::Duration;

/// Timer-based LLM alert evaluator.
/// Runs every 60 seconds, queries enabled LLM alert rules,
/// computes metrics from SQLite, checks cooldown, and dispatches alerts.
pub async fn llm_alert_evaluator(pool: Pool, dispatcher: Arc<AlertDispatcher>, cooldown_secs: u64) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));

    loop {
        interval.tick().await;

        if let Err(e) = evaluate_llm_alerts(&pool, &dispatcher, cooldown_secs).await {
            tracing::error!(error = %e, "llm alert evaluation failed");
        }
    }
}

/// A single rule row fetched from the alert_rules table.
#[derive(Debug)]
struct LlmAlertRule {
    id: i64,
    name: String,
    rule_type: String,
    config: String,
    project_id: Option<String>,
}

async fn evaluate_llm_alerts(
    pool: &Pool,
    dispatcher: &Arc<AlertDispatcher>,
    cooldown_secs: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let conn = pool.get().await?;

    let rules = conn
        .interact(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, rule_type, config, project_id
                 FROM alert_rules
                 WHERE enabled = 1
                   AND rule_type IN ('llm_cost_spike', 'llm_error_rate', 'llm_latency', 'llm_budget')",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(LlmAlertRule {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        rule_type: row.get(2)?,
                        config: row.get(3)?,
                        project_id: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| format!("interact error: {e}"))??;

    for rule in rules {
        let project_id = match &rule.project_id {
            Some(p) => p.clone(),
            None => continue, // LLM rules require a project_id
        };

        if let Err(e) =
            evaluate_single_rule(pool, dispatcher, &rule, &project_id, cooldown_secs).await
        {
            tracing::warn!(
                rule_id = rule.id,
                rule_name = %rule.name,
                error = %e,
                "failed to evaluate llm alert rule"
            );
        }
    }

    Ok(())
}

async fn evaluate_single_rule(
    pool: &Pool,
    dispatcher: &Arc<AlertDispatcher>,
    rule: &LlmAlertRule,
    project_id: &str,
    cooldown_secs: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match rule.rule_type.as_str() {
        "llm_cost_spike" => {
            let config: serde_json::Value = serde_json::from_str(&rule.config)?;
            let threshold_dollars = config["threshold_dollars"].as_f64().unwrap_or(0.0);
            let window_secs = config["window_secs"].as_u64().unwrap_or(3600);
            let model_filter = config["model_filter"].as_str().map(|s| s.to_string());

            let metric_key = "cost_hourly".to_string();
            if is_llm_cooldown(pool, project_id, rule.id, &metric_key, cooldown_secs).await {
                return Ok(());
            }

            let cost_micros =
                compute_cost_metric(pool, project_id, window_secs, model_filter.as_deref()).await?;
            let cost_dollars = cost_micros as f64 / 1_000_000.0;

            if cost_dollars > threshold_dollars {
                let window_str = format_window(window_secs);
                let message =
                    format_cost_alert(project_id, cost_dollars, threshold_dollars, &window_str);
                let channels = load_channels_for_rule(pool, rule.id).await;
                dispatcher
                    .dispatch_to_channels(&rule.name, &message, &channels)
                    .await;
                set_llm_cooldown(pool, project_id, rule.id, &metric_key).await;
            }
        }
        "llm_error_rate" => {
            let config: serde_json::Value = serde_json::from_str(&rule.config)?;
            let threshold_percent = config["threshold_percent"].as_f64().unwrap_or(0.0);
            let min_traces = config["min_traces"].as_u64().unwrap_or(10);
            let window_secs = config["window_secs"].as_u64().unwrap_or(3600);

            let metric_key = "error_rate".to_string();
            if is_llm_cooldown(pool, project_id, rule.id, &metric_key, cooldown_secs).await {
                return Ok(());
            }

            let (total, errors) = compute_error_rate_metric(pool, project_id, window_secs).await?;

            if total >= min_traces && total > 0 {
                let error_rate = (errors as f64 / total as f64) * 100.0;
                if error_rate > threshold_percent {
                    let message =
                        format_error_alert(project_id, error_rate, threshold_percent, total);
                    let channels = load_channels_for_rule(pool, rule.id).await;
                    dispatcher
                        .dispatch_to_channels(&rule.name, &message, &channels)
                        .await;
                    set_llm_cooldown(pool, project_id, rule.id, &metric_key).await;
                }
            }
        }
        "llm_latency" => {
            let config: serde_json::Value = serde_json::from_str(&rule.config)?;
            let percentile = config["percentile"].as_str().unwrap_or("p99").to_string();
            let threshold_ms = config["threshold_ms"].as_u64().unwrap_or(5000);
            let model_filter = config["model_filter"].as_str().map(|s| s.to_string());
            let window_secs = config["window_secs"].as_u64().unwrap_or(3600);

            let model_label = model_filter.as_deref().unwrap_or("all models");
            let metric_key = format!("latency_{}:{}", percentile, model_label);
            if is_llm_cooldown(pool, project_id, rule.id, &metric_key, cooldown_secs).await {
                return Ok(());
            }

            let latency_value = compute_latency_metric(
                pool,
                project_id,
                window_secs,
                &percentile,
                model_filter.as_deref(),
            )
            .await?;

            if latency_value > threshold_ms {
                let message = format_latency_alert(
                    project_id,
                    &percentile,
                    latency_value,
                    threshold_ms,
                    model_label,
                );
                let channels = load_channels_for_rule(pool, rule.id).await;
                dispatcher
                    .dispatch_to_channels(&rule.name, &message, &channels)
                    .await;
                set_llm_cooldown(pool, project_id, rule.id, &metric_key).await;
            }
        }
        "llm_budget" => {
            let metric_key = "budget_monthly".to_string();
            if is_llm_cooldown(pool, project_id, rule.id, &metric_key, cooldown_secs).await {
                return Ok(());
            }

            let conn = pool.get().await?;
            let pid = project_id.to_string();

            let (budget_micros, threshold_pct, spend_micros) = conn
                .interact(move |conn| {
                    let (budget, threshold): (i64, i64) = conn
                        .query_row(
                            "SELECT monthly_budget_micros, alert_threshold_pct
                             FROM llm_cost_budgets WHERE project_id = ?1",
                            params![pid],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .unwrap_or((0, 80));

                    if budget <= 0 {
                        return Ok::<_, rusqlite::Error>((0i64, 80i64, 0i64));
                    }

                    let now = chrono::Utc::now();
                    let month_start = now.date_naive().with_day(1).unwrap_or(now.date_naive());
                    let month_start_ms = month_start
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_utc()
                        .timestamp_millis();

                    let spend: i64 = conn
                        .query_row(
                            "SELECT COALESCE(SUM(cost_micros), 0)
                             FROM llm_usage_hourly
                             WHERE project_id = ?1 AND hour_bucket >= ?2",
                            params![pid, month_start_ms],
                            |row| row.get(0),
                        )
                        .unwrap_or(0);

                    Ok((budget, threshold, spend))
                })
                .await
                .map_err(|e| format!("interact error: {e}"))??;

            if budget_micros > 0 {
                let used_pct = (spend_micros as f64 / budget_micros as f64) * 100.0;
                if used_pct >= threshold_pct as f64 {
                    let message = format!(
                        "LLM Budget Alert: Project {} has used {:.1}% of monthly budget (${:.4} / ${:.4}, threshold: {}%)",
                        project_id,
                        used_pct,
                        spend_micros as f64 / 1_000_000.0,
                        budget_micros as f64 / 1_000_000.0,
                        threshold_pct
                    );
                    let channels = load_channels_for_rule(pool, rule.id).await;
                    dispatcher
                        .dispatch_to_channels(&rule.name, &message, &channels)
                        .await;
                    set_llm_cooldown(pool, project_id, rule.id, &metric_key).await;
                }
            }
        }
        _ => {
            tracing::warn!(rule_type = %rule.rule_type, "unknown llm alert rule type");
        }
    }

    Ok(())
}

// ── Metric computation ──

async fn compute_cost_metric(
    pool: &Pool,
    project_id: &str,
    window_secs: u64,
    model_filter: Option<&str>,
) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
    let conn = pool.get().await?;
    let pid = project_id.to_string();
    let cutoff = chrono::Utc::now().timestamp_millis() - (window_secs as i64 * 1000);
    let model = model_filter.map(|s| s.to_string());

    let cost = conn
        .interact(move |conn| {
            let cost: i64 = conn.query_row(
                "SELECT COALESCE(SUM(cost_micros), 0)
                     FROM llm_usage_hourly
                     WHERE project_id = ?1
                       AND hour_bucket >= ?2
                       AND (?3 IS NULL OR model = ?3)",
                params![pid, cutoff, model],
                |row| row.get(0),
            )?;
            Ok::<_, rusqlite::Error>(cost)
        })
        .await
        .map_err(|e| format!("interact error: {e}"))??;

    Ok(cost)
}

async fn compute_error_rate_metric(
    pool: &Pool,
    project_id: &str,
    window_secs: u64,
) -> Result<(u64, u64), Box<dyn std::error::Error + Send + Sync>> {
    let conn = pool.get().await?;
    let pid = project_id.to_string();
    let cutoff = chrono::Utc::now().timestamp_millis() - (window_secs as i64 * 1000);

    let result = conn
        .interact(move |conn| {
            let (total, errors): (i64, i64) = conn.query_row(
                "SELECT COUNT(*), SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END)
                 FROM llm_spans
                 WHERE project_id = ?1 AND started_at >= ?2",
                params![pid, cutoff],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok::<_, rusqlite::Error>((total as u64, errors as u64))
        })
        .await
        .map_err(|e| format!("interact error: {e}"))??;

    Ok(result)
}

async fn compute_latency_metric(
    pool: &Pool,
    project_id: &str,
    window_secs: u64,
    percentile: &str,
    model_filter: Option<&str>,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let conn = pool.get().await?;
    let pid = project_id.to_string();
    let cutoff = chrono::Utc::now().timestamp_millis() - (window_secs as i64 * 1000);
    let model = model_filter.map(|s| s.to_string());
    let pct_str = percentile.to_string();

    let latency = conn
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT latency_ms
                 FROM llm_spans
                 WHERE project_id = ?1
                   AND started_at >= ?2
                   AND (?3 IS NULL OR model = ?3)
                 ORDER BY latency_ms ASC",
            )?;
            let latencies: Vec<u64> = stmt
                .query_map(params![pid, cutoff, model], |row| {
                    row.get::<_, i64>(0).map(|v| v as u64)
                })?
                .filter_map(|r| r.ok())
                .collect();

            if latencies.is_empty() {
                return Ok::<_, rusqlite::Error>(0u64);
            }

            let pct_value = parse_percentile(&pct_str);
            let idx = ((pct_value / 100.0) * (latencies.len() as f64 - 1.0)).ceil() as usize;
            let idx = idx.min(latencies.len() - 1);
            Ok(latencies[idx])
        })
        .await
        .map_err(|e| format!("interact error: {e}"))??;

    Ok(latency)
}

/// Parse a percentile string like "p50", "p95", "p99" into a float (50.0, 95.0, 99.0).
fn parse_percentile(s: &str) -> f64 {
    let stripped = s.strip_prefix('p').unwrap_or(s);
    stripped.parse::<f64>().unwrap_or(99.0)
}

// ── Cooldown functions ──

async fn is_llm_cooldown(
    pool: &Pool,
    project_id: &str,
    rule_id: i64,
    metric_key: &str,
    cooldown_secs: u64,
) -> bool {
    let pid = project_id.to_string();
    let mk = metric_key.to_string();
    let conn = match pool.get().await {
        Ok(c) => c,
        Err(_) => return false,
    };

    let now = chrono::Utc::now().timestamp();
    let threshold = now - cooldown_secs as i64;

    conn.interact(move |conn| {
        let result: Option<i64> = conn
            .query_row(
                "SELECT last_fired FROM llm_alert_cooldowns
                 WHERE project_id = ?1 AND rule_id = ?2 AND metric_key = ?3",
                params![pid, rule_id, mk],
                |row| row.get(0),
            )
            .ok();

        match result {
            Some(last_fired) => last_fired > threshold,
            None => false,
        }
    })
    .await
    .unwrap_or(false)
}

async fn set_llm_cooldown(pool: &Pool, project_id: &str, rule_id: i64, metric_key: &str) {
    let pid = project_id.to_string();
    let mk = metric_key.to_string();
    let now = chrono::Utc::now().timestamp();
    if let Ok(conn) = pool.get().await {
        let _ = conn
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO llm_alert_cooldowns (project_id, rule_id, metric_key, last_fired)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (project_id, rule_id, metric_key) DO UPDATE SET last_fired = ?4",
                    params![pid, rule_id, mk, now],
                )
            })
            .await;
    }
}

// ── Alert message formatting ──

fn format_cost_alert(project: &str, cost_dollars: f64, threshold: f64, window: &str) -> String {
    format!(
        "LLM Cost Alert: Project {} spent ${:.4} in the last {} (threshold: ${:.4})",
        project, cost_dollars, window, threshold
    )
}

fn format_error_alert(project: &str, error_rate: f64, threshold: f64, total: u64) -> String {
    format!(
        "LLM Error Rate Alert: Project {} has {:.1}% error rate ({} traces, threshold: {:.1}%)",
        project, error_rate, total, threshold
    )
}

fn format_latency_alert(
    project: &str,
    percentile: &str,
    value_ms: u64,
    threshold_ms: u64,
    model: &str,
) -> String {
    format!(
        "LLM Latency Alert: Project {} {} is {}ms for {} (threshold: {}ms)",
        project, percentile, value_ms, model, threshold_ms
    )
}

fn format_window(secs: u64) -> String {
    if secs >= 3600 {
        let hours = secs / 3600;
        if hours == 1 {
            "1 hour".to_string()
        } else {
            format!("{} hours", hours)
        }
    } else {
        let mins = secs / 60;
        if mins == 1 {
            "1 minute".to_string()
        } else {
            format!("{} minutes", mins)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Format tests ──

    #[test]
    fn test_format_cost_alert() {
        let msg = format_cost_alert("my-project", 12.5678, 10.0, "1 hour");
        assert_eq!(
            msg,
            "LLM Cost Alert: Project my-project spent $12.5678 in the last 1 hour (threshold: $10.0000)"
        );
    }

    #[test]
    fn test_format_error_alert() {
        let msg = format_error_alert("my-project", 15.3, 10.0, 200);
        assert_eq!(
            msg,
            "LLM Error Rate Alert: Project my-project has 15.3% error rate (200 traces, threshold: 10.0%)"
        );
    }

    #[test]
    fn test_format_latency_alert() {
        let msg = format_latency_alert("my-project", "p99", 5200, 5000, "gpt-4");
        assert_eq!(
            msg,
            "LLM Latency Alert: Project my-project p99 is 5200ms for gpt-4 (threshold: 5000ms)"
        );
    }

    #[test]
    fn test_format_latency_alert_all_models() {
        let msg = format_latency_alert("my-project", "p95", 3000, 2000, "all models");
        assert_eq!(
            msg,
            "LLM Latency Alert: Project my-project p95 is 3000ms for all models (threshold: 2000ms)"
        );
    }

    // ── parse_percentile tests ──

    #[test]
    fn test_parse_percentile_p99() {
        assert!((parse_percentile("p99") - 99.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_percentile_p50() {
        assert!((parse_percentile("p50") - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_percentile_p95() {
        assert!((parse_percentile("p95") - 95.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_percentile_bare_number() {
        assert!((parse_percentile("75") - 75.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_percentile_invalid_defaults_99() {
        assert!((parse_percentile("garbage") - 99.0).abs() < f64::EPSILON);
    }

    // ── format_window tests ──

    #[test]
    fn test_format_window_1_hour() {
        assert_eq!(format_window(3600), "1 hour");
    }

    #[test]
    fn test_format_window_multiple_hours() {
        assert_eq!(format_window(7200), "2 hours");
    }

    #[test]
    fn test_format_window_1_minute() {
        assert_eq!(format_window(60), "1 minute");
    }

    #[test]
    fn test_format_window_multiple_minutes() {
        assert_eq!(format_window(300), "5 minutes");
    }
}
