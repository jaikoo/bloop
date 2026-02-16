use crate::types::ProcessedEvent;
use deadpool_sqlite::Pool;
use rusqlite::params;

/// Batch-write events to SQLite in a single transaction.
pub async fn write_batch(
    pool: &Pool,
    events: Vec<ProcessedEvent>,
    sample_reservoir_size: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if events.is_empty() {
        return Ok(());
    }

    let conn = pool.get().await?;
    conn.interact(move |conn| {
        let tx = conn.transaction()?;

        {
            let mut insert_raw = tx.prepare_cached(
                "INSERT INTO raw_events (
                    timestamp, source, environment, release, app_version,
                    build_number, route_or_procedure, screen, error_type,
                    message, stack, http_status, request_id, user_id_hash,
                    device_id_hash, fingerprint, metadata, received_at, project_id
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            )?;

            let mut upsert_agg = tx.prepare_cached(
                "INSERT INTO error_aggregates (
                    project_id, fingerprint, release, environment,
                    total_count, first_seen, last_seen,
                    error_type, message, source, route_or_procedure, screen, status
                ) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5, ?6, ?7, ?8, ?9, ?10, 'unresolved')
                ON CONFLICT (project_id, fingerprint, release, environment) DO UPDATE SET
                    total_count = total_count + 1,
                    last_seen   = excluded.last_seen,
                    status      = CASE WHEN status = 'resolved' THEN 'unresolved' ELSE status END",
            )?;

            let mut insert_sample = tx.prepare_cached(
                "INSERT INTO sample_occurrences (
                    fingerprint, captured_at, source, environment, release,
                    error_type, message, stack, request_id, metadata, project_id,
                    user_id_hash, device_id_hash, app_version, build_number,
                    screen, http_status, route_or_procedure
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            )?;

            let mut upsert_hourly = tx.prepare_cached(
                "INSERT INTO event_counts_hourly (project_id, fingerprint, hour_bucket, environment, source, count)
                VALUES (?1, ?2, ?3, ?4, ?5, 1)
                ON CONFLICT (project_id, fingerprint, hour_bucket, environment, source) DO UPDATE SET count = count + 1",
            )?;

            let mut count_samples = tx.prepare_cached(
                "SELECT COUNT(*) FROM sample_occurrences WHERE fingerprint = ?1 AND project_id = ?2",
            )?;

            let mut prune_samples = tx.prepare_cached(
                "DELETE FROM sample_occurrences WHERE fingerprint = ?1 AND project_id = ?2 AND id NOT IN (
                    SELECT id FROM sample_occurrences WHERE fingerprint = ?1 AND project_id = ?2
                    ORDER BY captured_at DESC LIMIT ?3
                )",
            )?;

            for pe in &events {
                let e = &pe.event;
                let metadata_str = e.metadata.as_ref().map(|v| v.to_string());

                insert_raw.execute(params![
                    e.timestamp,
                    e.source.as_str(),
                    e.environment,
                    e.release,
                    e.app_version,
                    e.build_number,
                    e.route_or_procedure,
                    e.screen,
                    e.error_type,
                    e.message,
                    e.stack,
                    e.http_status,
                    e.request_id,
                    e.user_id_hash,
                    e.device_id_hash,
                    pe.fingerprint,
                    metadata_str,
                    pe.received_at,
                    pe.project_id,
                ])?;

                upsert_agg.execute(params![
                    pe.project_id,
                    pe.fingerprint,
                    e.release,
                    e.environment,
                    pe.received_at,
                    e.error_type,
                    e.message,
                    e.source.as_str(),
                    e.route_or_procedure,
                    e.screen,
                ])?;

                // Hourly bucketing for trend charts
                let hour_bucket = (pe.received_at / 3_600_000) * 3_600_000;
                upsert_hourly.execute(params![
                    pe.project_id,
                    pe.fingerprint,
                    hour_bucket,
                    e.environment,
                    e.source.as_str(),
                ])?;

                // Sample reservoir: insert if under limit, then prune
                let sample_count: i64 = count_samples
                    .query_row(params![pe.fingerprint, pe.project_id], |row| row.get(0))?;

                let sample_params = params![
                    pe.fingerprint,
                    pe.received_at,
                    e.source.as_str(),
                    e.environment,
                    e.release,
                    e.error_type,
                    e.message,
                    e.stack,
                    e.request_id,
                    metadata_str,
                    pe.project_id,
                    e.user_id_hash,
                    e.device_id_hash,
                    e.app_version,
                    e.build_number,
                    e.screen,
                    e.http_status,
                    e.route_or_procedure,
                ];

                if sample_count < sample_reservoir_size as i64 {
                    insert_sample.execute(sample_params)?;
                } else {
                    // Always insert, then prune to N
                    insert_sample.execute(sample_params)?;
                    prune_samples.execute(params![
                        pe.fingerprint,
                        pe.project_id,
                        sample_reservoir_size as i64,
                    ])?;
                }
            }
        }

        tx.commit()?;
        tracing::debug!(count = events.len(), "flushed batch to sqlite");
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| format!("interact error: {e}"))??;

    Ok(())
}
