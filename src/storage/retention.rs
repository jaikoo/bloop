use deadpool_sqlite::Pool;
use rusqlite::params;
use std::time::Duration;
use tokio::time;

/// Background task that prunes old events based on configurable retention settings.
pub async fn retention_loop(pool: Pool, default_retention_days: u64, interval_secs: u64) {
    let mut interval = time::interval(Duration::from_secs(interval_secs));

    loop {
        interval.tick().await;

        match run_retention_once(&pool, default_retention_days).await {
            Ok((raw_deleted, hourly_deleted)) => {
                if raw_deleted > 0 {
                    tracing::info!(deleted = raw_deleted, "pruned old raw events");
                }
                if hourly_deleted > 0 {
                    tracing::info!(deleted = hourly_deleted, "pruned old hourly counts");
                }
            }
            Err(e) => tracing::error!(error = %e, "retention prune failed"),
        }
    }
}

/// Run a single retention pass. Returns (raw_deleted, hourly_deleted).
pub async fn run_retention_once(
    pool: &Pool,
    default_retention_days: u64,
) -> Result<(usize, usize), Box<dyn std::error::Error + Send + Sync>> {
    let conn = pool.get().await?;

    let result = conn
        .interact(move |conn| {
            let now_ms = chrono::Utc::now().timestamp_millis();

            // Read configurable global settings (fall back to defaults)
            let global_raw_days: u64 = conn
                .query_row(
                    "SELECT value FROM retention_settings WHERE key = 'raw_events_days'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default_retention_days);

            let global_hourly_days: u64 = conn
                .query_row(
                    "SELECT value FROM retention_settings WHERE key = 'hourly_counts_days'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(90);

            let mut total_raw_deleted: usize = 0;

            // Per-project retention overrides
            let project_overrides: Vec<(String, u64)> = {
                let mut result = Vec::new();
                if let Ok(mut stmt) =
                    conn.prepare("SELECT project_id, raw_events_days FROM project_retention")
                {
                    if let Ok(rows) = stmt.query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
                    }) {
                        for row in rows.flatten() {
                            result.push(row);
                        }
                    }
                }
                result
            };

            // Delete per-project with custom retention
            for (project_id, days) in &project_overrides {
                let cutoff_ms = now_ms - (*days as i64 * 86400 * 1000);
                let deleted = conn
                    .execute(
                        "DELETE FROM raw_events WHERE project_id = ?1 AND timestamp < ?2",
                        params![project_id, cutoff_ms],
                    )
                    .unwrap_or(0);
                total_raw_deleted += deleted;
            }

            // Delete remaining (global retention) — exclude projects with overrides
            let global_cutoff_ms = now_ms - (global_raw_days as i64 * 86400 * 1000);
            if project_overrides.is_empty() {
                let deleted = conn
                    .execute(
                        "DELETE FROM raw_events WHERE timestamp < ?1",
                        params![global_cutoff_ms],
                    )
                    .unwrap_or(0);
                total_raw_deleted += deleted;
            } else {
                let placeholders: Vec<String> = project_overrides
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", i + 2))
                    .collect();
                let sql = format!(
                    "DELETE FROM raw_events WHERE timestamp < ?1 AND project_id NOT IN ({})",
                    placeholders.join(",")
                );
                let mut stmt = conn.prepare(&sql)?;
                let mut bind_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                bind_params.push(Box::new(global_cutoff_ms));
                for (pid, _) in &project_overrides {
                    bind_params.push(Box::new(pid.clone()));
                }
                let refs: Vec<&dyn rusqlite::types::ToSql> =
                    bind_params.iter().map(|b| b.as_ref()).collect();
                let deleted = stmt.execute(refs.as_slice()).unwrap_or(0);
                total_raw_deleted += deleted;
            }

            // Hourly counts retention (global only)
            let hourly_cutoff_ms = now_ms - (global_hourly_days as i64 * 86400 * 1000);
            let hourly_deleted = conn
                .execute(
                    "DELETE FROM event_counts_hourly WHERE hour_bucket < ?1",
                    params![hourly_cutoff_ms],
                )
                .unwrap_or(0);

            // LLM tracing retention (same cadence as hourly counts)
            let llm_deleted = prune_llm_tables(conn, hourly_cutoff_ms);

            // VACUUM after large deletions
            if total_raw_deleted > 10000 || hourly_deleted > 10000 || llm_deleted > 10000 {
                let _ = conn.execute_batch("VACUUM");
            }

            Ok::<_, rusqlite::Error>((total_raw_deleted, hourly_deleted))
        })
        .await
        .map_err(|e| format!("interact error: {e}"))??;

    Ok(result)
}

/// Prune LLM tracing tables older than the cutoff.
/// Tables only exist after migration 012; gracefully ignores missing tables.
fn prune_llm_tables(conn: &rusqlite::Connection, cutoff_ms: i64) -> usize {
    let mut total = 0usize;

    // Delete old spans (by started_at)
    total += conn
        .execute(
            "DELETE FROM llm_spans WHERE started_at < ?1",
            params![cutoff_ms],
        )
        .unwrap_or(0);

    // Delete old traces (by started_at)
    total += conn
        .execute(
            "DELETE FROM llm_traces WHERE started_at < ?1",
            params![cutoff_ms],
        )
        .unwrap_or(0);

    // Delete old hourly usage
    total += conn
        .execute(
            "DELETE FROM llm_usage_hourly WHERE hour_bucket < ?1",
            params![cutoff_ms],
        )
        .unwrap_or(0);

    // Delete old LLM alert cooldowns
    total += conn
        .execute(
            "DELETE FROM llm_alert_cooldowns WHERE last_fired < ?1",
            params![cutoff_ms / 1000], // cooldowns use unix seconds, not millis
        )
        .unwrap_or(0);

    // Delete old LLM trace scores
    total += conn
        .execute(
            "DELETE FROM llm_trace_scores WHERE created_at < ?1",
            params![cutoff_ms],
        )
        .unwrap_or(0);

    if total > 0 {
        tracing::info!(deleted = total, "pruned old llm tracing data");
    }

    total
}
