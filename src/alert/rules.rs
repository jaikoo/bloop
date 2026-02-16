use crate::error::{AppError, AppResult};
use crate::pipeline::worker::NewFingerprintEvent;
use deadpool_sqlite::Pool;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertRuleConfig {
    /// Fire when a new fingerprint appears
    NewIssue { environment: Option<String> },
    /// Fire when error count exceeds threshold in window
    Threshold {
        fingerprint: Option<String>,
        route: Option<String>,
        threshold: u64,
        window_secs: u64,
    },
    /// Fire when error rate spikes vs rolling baseline
    Spike {
        multiplier: f64,
        baseline_window_secs: u64,
        compare_window_secs: u64,
    },
    /// Fire when LLM cost exceeds threshold in window
    #[cfg(feature = "llm-tracing")]
    LlmCostSpike {
        threshold_dollars: f64,
        window_secs: u64,
        model_filter: Option<String>,
    },
    /// Fire when LLM error rate exceeds threshold
    #[cfg(feature = "llm-tracing")]
    LlmErrorRate {
        threshold_percent: f64,
        min_traces: u64,
        window_secs: u64,
    },
    /// Fire when LLM latency percentile exceeds threshold
    #[cfg(feature = "llm-tracing")]
    LlmLatency {
        percentile: String,
        threshold_ms: u64,
        model_filter: Option<String>,
        window_secs: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    pub id: i64,
    pub name: String,
    pub rule_type: String,
    pub config: AlertRuleConfig,
    pub enabled: bool,
    pub project_id: Option<String>,
    pub description: Option<String>,
    pub environment_filter: Option<String>,
    pub source_filter: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateAlertRule {
    pub name: String,
    pub config: AlertRuleConfig,
    pub project_id: Option<String>,
    pub description: Option<String>,
    pub environment_filter: Option<String>,
    pub source_filter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertChannel {
    pub id: i64,
    pub rule_id: i64,
    pub channel_type: String,
    pub config: super::dispatch::ChannelConfig,
    pub created_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateAlertChannel {
    pub config: super::dispatch::ChannelConfig,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAlertRule {
    pub name: Option<String>,
    pub config: Option<AlertRuleConfig>,
    pub enabled: Option<bool>,
    pub description: Option<String>,
    pub environment_filter: Option<String>,
    pub source_filter: Option<String>,
}

/// Alert evaluator: listens for new fingerprints and evaluates threshold/spike rules.
pub async fn alert_evaluator(
    mut new_fp_rx: mpsc::UnboundedReceiver<NewFingerprintEvent>,
    pool: Pool,
    dispatcher: Arc<super::dispatch::AlertDispatcher>,
    cooldown_secs: u64,
) {
    while let Some(event) = new_fp_rx.recv().await {
        // Evaluate new-issue rules
        if let Err(e) = evaluate_new_issue(&pool, &event, &dispatcher, cooldown_secs).await {
            tracing::error!(error = %e, "alert evaluation failed");
        }
    }
}

async fn evaluate_new_issue(
    pool: &Pool,
    event: &NewFingerprintEvent,
    dispatcher: &Arc<super::dispatch::AlertDispatcher>,
    cooldown_secs: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let env = event.environment.clone();
    let fp = event.fingerprint.clone();
    let project_id = event.project_id.clone();

    let conn = pool.get().await?;
    let pid = project_id.clone();
    let rules = conn
        .interact(move |conn| {
            // Match rules for this project or global rules (NULL project_id)
            let mut stmt = conn.prepare(
                "SELECT id, name, rule_type, config, enabled, created_at, updated_at
                 FROM alert_rules WHERE enabled = 1 AND rule_type = 'new_issue'
                 AND (project_id = ?1 OR project_id IS NULL)",
            )?;
            let rows = stmt
                .query_map(params![pid], |row| {
                    let config_str: String = row.get(3)?;
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, config_str))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| format!("interact error: {e}"))??;

    for (rule_id, rule_name, config_str) in rules {
        let config: AlertRuleConfig = match serde_json::from_str(&config_str) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(rule_id, error = %e, "invalid alert rule config");
                continue;
            }
        };

        if let AlertRuleConfig::NewIssue { environment } = &config {
            if let Some(ref required_env) = environment {
                if required_env != &env {
                    continue;
                }
            }

            // Check cooldown (scoped by project)
            if is_on_cooldown(pool, &project_id, rule_id, &fp, cooldown_secs).await {
                continue;
            }

            let message = format!(
                "New error in {} ({}): [{}] {} - {}",
                event.release,
                event.environment,
                event.error_type,
                event.message,
                event.fingerprint
            );

            let channels = load_channels_for_rule(pool, rule_id).await;
            dispatcher
                .dispatch_to_channels(&rule_name, &message, &channels)
                .await;
            set_cooldown(pool, &project_id, rule_id, &fp).await;
        }
    }

    Ok(())
}

async fn is_on_cooldown(
    pool: &Pool,
    project_id: &str,
    rule_id: i64,
    fingerprint: &str,
    cooldown_secs: u64,
) -> bool {
    let fp = fingerprint.to_string();
    let pid = project_id.to_string();
    let conn = match pool.get().await {
        Ok(c) => c,
        Err(_) => return false,
    };

    let now = chrono::Utc::now().timestamp();
    let threshold = now - cooldown_secs as i64;

    conn.interact(move |conn| {
        let result: Option<i64> = conn
            .query_row(
                "SELECT last_fired FROM alert_cooldowns WHERE project_id = ?1 AND rule_id = ?2 AND fingerprint = ?3",
                params![pid, rule_id, fp],
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

async fn set_cooldown(pool: &Pool, project_id: &str, rule_id: i64, fingerprint: &str) {
    let fp = fingerprint.to_string();
    let pid = project_id.to_string();
    let now = chrono::Utc::now().timestamp();
    if let Ok(conn) = pool.get().await {
        let _ = conn
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO alert_cooldowns (project_id, rule_id, fingerprint, last_fired)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (project_id, rule_id, fingerprint) DO UPDATE SET last_fired = ?4",
                    params![pid, rule_id, fp, now],
                )
            })
            .await;
    }
}

// --- CRUD handlers for alert rules ---

pub async fn list_alert_rules(pool: &Pool) -> AppResult<Vec<AlertRule>> {
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let rules = conn
        .interact(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, rule_type, config, enabled, created_at, updated_at, project_id, description, environment_filter, source_filter
                 FROM alert_rules ORDER BY id",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    let config_str: String = row.get(3)?;
                    let config: AlertRuleConfig =
                        serde_json::from_str(&config_str).unwrap_or(AlertRuleConfig::NewIssue {
                            environment: None,
                        });
                    Ok(AlertRule {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        rule_type: row.get(2)?,
                        config,
                        enabled: row.get::<_, i64>(4)? != 0,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
                        project_id: row.get(7)?,
                        description: row.get(8)?,
                        environment_filter: row.get(9)?,
                        source_filter: row.get(10)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(rules)
}

pub async fn create_alert_rule(pool: &Pool, input: CreateAlertRule) -> AppResult<AlertRule> {
    let rule_type = match &input.config {
        AlertRuleConfig::NewIssue { .. } => "new_issue",
        AlertRuleConfig::Threshold { .. } => "threshold",
        AlertRuleConfig::Spike { .. } => "spike",
        #[cfg(feature = "llm-tracing")]
        AlertRuleConfig::LlmCostSpike { .. } => "llm_cost_spike",
        #[cfg(feature = "llm-tracing")]
        AlertRuleConfig::LlmErrorRate { .. } => "llm_error_rate",
        #[cfg(feature = "llm-tracing")]
        AlertRuleConfig::LlmLatency { .. } => "llm_latency",
    };

    let config_json = serde_json::to_string(&input.config)
        .map_err(|e| AppError::Internal(format!("json error: {e}")))?;

    let now = chrono::Utc::now().timestamp();
    let name = input.name.clone();
    let rt = rule_type.to_string();
    let cj = config_json.clone();
    let pid = input.project_id.clone();
    let desc = input.description.clone();
    let env_filter = input.environment_filter.clone();
    let src_filter = input.source_filter.clone();

    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let id = conn
        .interact(move |conn| {
            conn.execute(
                "INSERT INTO alert_rules (name, rule_type, config, enabled, created_at, updated_at, project_id, description, environment_filter, source_filter)
                 VALUES (?1, ?2, ?3, 1, ?4, ?4, ?5, ?6, ?7, ?8)",
                params![name, rt, cj, now, pid, desc, env_filter, src_filter],
            )?;
            Ok::<_, rusqlite::Error>(conn.last_insert_rowid())
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(AlertRule {
        id,
        name: input.name,
        rule_type: rule_type.to_string(),
        config: input.config,
        enabled: true,
        project_id: input.project_id,
        description: input.description,
        environment_filter: input.environment_filter,
        source_filter: input.source_filter,
        created_at: now,
        updated_at: now,
    })
}

pub async fn delete_alert_rule(pool: &Pool, id: i64) -> AppResult<()> {
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let deleted = conn
        .interact(move |conn| conn.execute("DELETE FROM alert_rules WHERE id = ?1", params![id]))
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    if deleted == 0 {
        return Err(AppError::NotFound(format!("alert rule {id} not found")));
    }

    Ok(())
}

pub async fn update_alert_rule(
    pool: &Pool,
    id: i64,
    input: UpdateAlertRule,
) -> AppResult<AlertRule> {
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let now = chrono::Utc::now().timestamp();

    let rule = conn
        .interact(move |conn| {
            // Build dynamic UPDATE
            let mut sets = vec!["updated_at = ?1".to_string()];
            let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            bind_values.push(Box::new(now));

            if let Some(ref name) = input.name {
                sets.push(format!("name = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(name.clone()));
            }
            if let Some(ref config) = input.config {
                let config_json = serde_json::to_string(config).unwrap_or_default();
                let rule_type = match config {
                    AlertRuleConfig::NewIssue { .. } => "new_issue",
                    AlertRuleConfig::Threshold { .. } => "threshold",
                    AlertRuleConfig::Spike { .. } => "spike",
                    #[cfg(feature = "llm-tracing")]
                    AlertRuleConfig::LlmCostSpike { .. } => "llm_cost_spike",
                    #[cfg(feature = "llm-tracing")]
                    AlertRuleConfig::LlmErrorRate { .. } => "llm_error_rate",
                    #[cfg(feature = "llm-tracing")]
                    AlertRuleConfig::LlmLatency { .. } => "llm_latency",
                };
                sets.push(format!("config = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(config_json));
                sets.push(format!("rule_type = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(rule_type.to_string()));
            }
            if let Some(enabled) = input.enabled {
                sets.push(format!("enabled = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(enabled as i64));
            }
            if let Some(ref desc) = input.description {
                sets.push(format!("description = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(desc.clone()));
            }
            if let Some(ref env_filter) = input.environment_filter {
                sets.push(format!("environment_filter = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(env_filter.clone()));
            }
            if let Some(ref src_filter) = input.source_filter {
                sets.push(format!("source_filter = ?{}", bind_values.len() + 1));
                bind_values.push(Box::new(src_filter.clone()));
            }

            let id_param = bind_values.len() + 1;
            bind_values.push(Box::new(id));

            let sql = format!("UPDATE alert_rules SET {} WHERE id = ?{}", sets.join(", "), id_param);
            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                bind_values.iter().map(|b| b.as_ref()).collect();
            let updated = conn.execute(&sql, params_ref.as_slice())?;

            if updated == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }

            // Read back the updated rule
            let mut stmt = conn.prepare(
                "SELECT id, name, rule_type, config, enabled, created_at, updated_at, project_id, description, environment_filter, source_filter
                 FROM alert_rules WHERE id = ?1",
            )?;
            stmt.query_row(params![id], |row| {
                let config_str: String = row.get(3)?;
                let config: AlertRuleConfig =
                    serde_json::from_str(&config_str).unwrap_or(AlertRuleConfig::NewIssue {
                        environment: None,
                    });
                Ok(AlertRule {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    rule_type: row.get(2)?,
                    config,
                    enabled: row.get::<_, i64>(4)? != 0,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                    project_id: row.get(7)?,
                    description: row.get(8)?,
                    environment_filter: row.get(9)?,
                    source_filter: row.get(10)?,
                })
            })
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(rule)
}

// --- Channel CRUD ---

pub async fn list_channels(pool: &Pool, rule_id: i64) -> AppResult<Vec<AlertChannel>> {
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let channels = conn
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, rule_id, channel_type, config, created_at
                 FROM alert_channels WHERE rule_id = ?1 ORDER BY id",
            )?;
            let rows = stmt
                .query_map(params![rule_id], |row| {
                    let config_str: String = row.get(3)?;
                    let config: super::dispatch::ChannelConfig = serde_json::from_str(&config_str)
                        .unwrap_or(super::dispatch::ChannelConfig::Webhook { url: String::new() });
                    Ok(AlertChannel {
                        id: row.get(0)?,
                        rule_id: row.get(1)?,
                        channel_type: row.get(2)?,
                        config,
                        created_at: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(channels)
}

pub async fn add_channel(
    pool: &Pool,
    rule_id: i64,
    input: CreateAlertChannel,
) -> AppResult<AlertChannel> {
    // Validate channel config
    match &input.config {
        super::dispatch::ChannelConfig::Slack { webhook_url } => {
            super::dispatch::validate_webhook_url(webhook_url)
                .map_err(|e| AppError::Validation(format!("invalid channel URL: {e}")))?;
        }
        super::dispatch::ChannelConfig::Webhook { url } => {
            super::dispatch::validate_webhook_url(url)
                .map_err(|e| AppError::Validation(format!("invalid channel URL: {e}")))?;
        }
        super::dispatch::ChannelConfig::Email { to } => {
            if !to.contains('@') || to.len() < 3 {
                return Err(AppError::Validation("invalid email address".to_string()));
            }
        }
    }

    let channel_type = match &input.config {
        super::dispatch::ChannelConfig::Slack { .. } => "slack",
        super::dispatch::ChannelConfig::Webhook { .. } => "webhook",
        super::dispatch::ChannelConfig::Email { .. } => "email",
    };

    let config_json = serde_json::to_string(&input.config)
        .map_err(|e| AppError::Internal(format!("json error: {e}")))?;

    let now = chrono::Utc::now().timestamp();
    let ct = channel_type.to_string();
    let cj = config_json;

    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let id = conn
        .interact(move |conn| {
            conn.execute(
                "INSERT INTO alert_channels (rule_id, channel_type, config, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![rule_id, ct, cj, now],
            )?;
            Ok::<_, rusqlite::Error>(conn.last_insert_rowid())
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(AlertChannel {
        id,
        rule_id,
        channel_type: channel_type.to_string(),
        config: input.config,
        created_at: now,
    })
}

pub async fn delete_channel(pool: &Pool, rule_id: i64, channel_id: i64) -> AppResult<()> {
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let deleted = conn
        .interact(move |conn| {
            conn.execute(
                "DELETE FROM alert_channels WHERE id = ?1 AND rule_id = ?2",
                params![channel_id, rule_id],
            )
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    if deleted == 0 {
        return Err(AppError::NotFound(format!(
            "channel {channel_id} not found on rule {rule_id}"
        )));
    }

    Ok(())
}

/// Load channels for a given rule ID.
pub async fn load_channels_for_rule(
    pool: &Pool,
    rule_id: i64,
) -> Vec<super::dispatch::ChannelConfig> {
    match list_channels(pool, rule_id).await {
        Ok(channels) => channels.into_iter().map(|c| c.config).collect(),
        Err(_) => vec![],
    }
}

/// Send a test notification through a rule's channels.
pub async fn test_alert(
    pool: &Pool,
    rule_id: i64,
    dispatcher: &std::sync::Arc<super::dispatch::AlertDispatcher>,
) -> AppResult<()> {
    let channels = load_channels_for_rule(pool, rule_id).await;

    let message = format!(
        "Test notification from bloop alert rule #{} at {}",
        rule_id,
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    );

    dispatcher
        .dispatch_to_channels("test", &message, &channels)
        .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alert_rule_config_new_issue_serde() {
        let config = AlertRuleConfig::NewIssue {
            environment: Some("production".to_string()),
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: AlertRuleConfig = serde_json::from_str(&json).unwrap();
        match parsed {
            AlertRuleConfig::NewIssue { environment } => {
                assert_eq!(environment, Some("production".to_string()));
            }
            _ => panic!("expected NewIssue"),
        }
    }

    #[test]
    fn test_alert_rule_config_threshold_serde() {
        let config = AlertRuleConfig::Threshold {
            fingerprint: Some("abc123".to_string()),
            route: None,
            threshold: 100,
            window_secs: 3600,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: AlertRuleConfig = serde_json::from_str(&json).unwrap();
        match parsed {
            AlertRuleConfig::Threshold {
                fingerprint,
                threshold,
                window_secs,
                ..
            } => {
                assert_eq!(fingerprint, Some("abc123".to_string()));
                assert_eq!(threshold, 100);
                assert_eq!(window_secs, 3600);
            }
            _ => panic!("expected Threshold"),
        }
    }

    #[test]
    fn test_alert_rule_config_spike_serde() {
        let config = AlertRuleConfig::Spike {
            multiplier: 3.0,
            baseline_window_secs: 3600,
            compare_window_secs: 300,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: AlertRuleConfig = serde_json::from_str(&json).unwrap();
        match parsed {
            AlertRuleConfig::Spike {
                multiplier,
                baseline_window_secs,
                compare_window_secs,
            } => {
                assert!((multiplier - 3.0).abs() < f64::EPSILON);
                assert_eq!(baseline_window_secs, 3600);
                assert_eq!(compare_window_secs, 300);
            }
            _ => panic!("expected Spike"),
        }
    }

    #[test]
    fn test_new_issue_environment_filter() {
        let config = AlertRuleConfig::NewIssue {
            environment: Some("staging".to_string()),
        };
        if let AlertRuleConfig::NewIssue { environment } = &config {
            let required_env = environment.as_ref().unwrap();
            assert_eq!(required_env, "staging");
            assert_ne!(required_env, "production");
        }
    }

    #[test]
    fn test_channel_config_email_serde() {
        let config = super::super::dispatch::ChannelConfig::Email {
            to: "user@example.com".to_string(),
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("email"));
        assert!(json.contains("user@example.com"));
        let parsed: super::super::dispatch::ChannelConfig = serde_json::from_str(&json).unwrap();
        match parsed {
            super::super::dispatch::ChannelConfig::Email { to } => {
                assert_eq!(to, "user@example.com");
            }
            _ => panic!("expected Email"),
        }
    }

    #[cfg(feature = "llm-tracing")]
    #[test]
    fn test_alert_rule_config_llm_cost_spike_serde() {
        let config = AlertRuleConfig::LlmCostSpike {
            threshold_dollars: 50.0,
            window_secs: 3600,
            model_filter: Some("gpt-4".to_string()),
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: AlertRuleConfig = serde_json::from_str(&json).unwrap();
        match parsed {
            AlertRuleConfig::LlmCostSpike {
                threshold_dollars,
                window_secs,
                model_filter,
            } => {
                assert!((threshold_dollars - 50.0).abs() < f64::EPSILON);
                assert_eq!(window_secs, 3600);
                assert_eq!(model_filter, Some("gpt-4".to_string()));
            }
            _ => panic!("expected LlmCostSpike"),
        }
    }

    #[cfg(feature = "llm-tracing")]
    #[test]
    fn test_alert_rule_config_llm_error_rate_serde() {
        let config = AlertRuleConfig::LlmErrorRate {
            threshold_percent: 5.0,
            min_traces: 100,
            window_secs: 1800,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: AlertRuleConfig = serde_json::from_str(&json).unwrap();
        match parsed {
            AlertRuleConfig::LlmErrorRate {
                threshold_percent,
                min_traces,
                window_secs,
            } => {
                assert!((threshold_percent - 5.0).abs() < f64::EPSILON);
                assert_eq!(min_traces, 100);
                assert_eq!(window_secs, 1800);
            }
            _ => panic!("expected LlmErrorRate"),
        }
    }

    #[cfg(feature = "llm-tracing")]
    #[test]
    fn test_alert_rule_config_llm_latency_serde() {
        let config = AlertRuleConfig::LlmLatency {
            percentile: "p99".to_string(),
            threshold_ms: 5000,
            model_filter: None,
            window_secs: 7200,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: AlertRuleConfig = serde_json::from_str(&json).unwrap();
        match parsed {
            AlertRuleConfig::LlmLatency {
                percentile,
                threshold_ms,
                model_filter,
                window_secs,
            } => {
                assert_eq!(percentile, "p99");
                assert_eq!(threshold_ms, 5000);
                assert!(model_filter.is_none());
                assert_eq!(window_secs, 7200);
            }
            _ => panic!("expected LlmLatency"),
        }
    }
}
