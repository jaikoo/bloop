/// Spike detection: rolling-window z-score over hourly counts.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = environment (or NULL),
///             $4 = zscore_threshold, $5 = limit
pub const SPIKES_SQL: &str = r#"
WITH hourly AS (
    SELECT
        h.fingerprint,
        h.hour_bucket,
        h.count,
        a.error_type,
        a.message
    FROM bloop.event_counts_hourly h
    JOIN bloop.error_aggregates a
        ON h.fingerprint = a.fingerprint
        AND h.project_id = a.project_id
        AND h.environment = a.environment
    WHERE h.hour_bucket >= $1
        AND ($2 IS NULL OR h.project_id = $2)
        AND ($3 IS NULL OR h.environment = $3)
),
stats AS (
    SELECT
        fingerprint,
        error_type,
        message,
        hour_bucket,
        count,
        AVG(count) OVER (PARTITION BY fingerprint) AS mean,
        STDDEV_SAMP(count) OVER (PARTITION BY fingerprint) AS stddev
    FROM hourly
)
SELECT
    fingerprint,
    error_type,
    message,
    hour_bucket,
    count,
    mean,
    stddev,
    CASE WHEN stddev > 0 THEN (count - mean) / stddev ELSE 0 END AS zscore
FROM stats
WHERE stddev > 0
    AND (count - mean) / stddev >= $4
ORDER BY zscore DESC
LIMIT $5
"#;

/// Top movers: compare last N hours vs prior N hours.
/// Parameters: $1 = current_start_ms, $2 = current_end_ms, $3 = prev_start_ms,
///             $4 = project_id (or NULL), $5 = environment (or NULL), $6 = limit
pub const MOVERS_SQL: &str = r#"
WITH current_window AS (
    SELECT
        h.fingerprint,
        SUM(h.count) AS cnt,
        a.error_type,
        a.message
    FROM bloop.event_counts_hourly h
    JOIN bloop.error_aggregates a
        ON h.fingerprint = a.fingerprint
        AND h.project_id = a.project_id
        AND h.environment = a.environment
    WHERE h.hour_bucket >= $1 AND h.hour_bucket < $2
        AND ($4 IS NULL OR h.project_id = $4)
        AND ($5 IS NULL OR h.environment = $5)
    GROUP BY h.fingerprint, a.error_type, a.message
),
prev_window AS (
    SELECT
        h.fingerprint,
        SUM(h.count) AS cnt
    FROM bloop.event_counts_hourly h
    WHERE h.hour_bucket >= $3 AND h.hour_bucket < $1
        AND ($4 IS NULL OR h.project_id = $4)
        AND ($5 IS NULL OR h.environment = $5)
    GROUP BY h.fingerprint
)
SELECT
    COALESCE(c.fingerprint, p.fingerprint) AS fingerprint,
    COALESCE(c.error_type, '') AS error_type,
    COALESCE(c.message, '') AS message,
    COALESCE(c.cnt, 0) AS count_current,
    COALESCE(p.cnt, 0) AS count_previous,
    COALESCE(c.cnt, 0) - COALESCE(p.cnt, 0) AS delta,
    CASE WHEN COALESCE(p.cnt, 0) > 0
        THEN ((COALESCE(c.cnt, 0) - p.cnt) * 100.0 / p.cnt)
        ELSE CASE WHEN COALESCE(c.cnt, 0) > 0 THEN 100.0 ELSE 0.0 END
    END AS delta_pct
FROM current_window c
FULL OUTER JOIN prev_window p ON c.fingerprint = p.fingerprint
ORDER BY ABS(COALESCE(c.cnt, 0) - COALESCE(p.cnt, 0)) DESC
LIMIT $6
"#;

/// Correlation: pairwise correlation between fingerprints over hourly counts.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = environment (or NULL), $4 = limit
pub const CORRELATIONS_SQL: &str = r#"
WITH hourly AS (
    SELECT fingerprint, hour_bucket, SUM(count) AS count
    FROM bloop.event_counts_hourly
    WHERE hour_bucket >= $1
        AND ($2 IS NULL OR project_id = $2)
        AND ($3 IS NULL OR environment = $3)
    GROUP BY fingerprint, hour_bucket
    HAVING COUNT(*) >= 1
),
qualified AS (
    SELECT fingerprint
    FROM hourly
    GROUP BY fingerprint
    HAVING COUNT(*) >= 6
),
pairs AS (
    SELECT
        a.fingerprint AS fp_a,
        b.fingerprint AS fp_b,
        CORR(a.count, b.count) AS correlation
    FROM hourly a
    JOIN hourly b ON a.hour_bucket = b.hour_bucket AND a.fingerprint < b.fingerprint
    JOIN qualified qa ON a.fingerprint = qa.fingerprint
    JOIN qualified qb ON b.fingerprint = qb.fingerprint
    GROUP BY a.fingerprint, b.fingerprint
    HAVING CORR(a.count, b.count) >= 0.7
)
SELECT
    p.fp_a AS fingerprint_a,
    p.fp_b AS fingerprint_b,
    COALESCE(ma.message, '') AS message_a,
    COALESCE(mb.message, '') AS message_b,
    p.correlation
FROM pairs p
LEFT JOIN bloop.error_aggregates ma ON p.fp_a = ma.fingerprint
    AND ($2 IS NULL OR ma.project_id = $2)
LEFT JOIN bloop.error_aggregates mb ON p.fp_b = mb.fingerprint
    AND ($2 IS NULL OR mb.project_id = $2)
ORDER BY p.correlation DESC
LIMIT $4
"#;

/// Release impact: new fingerprints per release, composite impact score.
/// Parameters: $1 = project_id (or NULL), $2 = limit
pub const RELEASES_SQL: &str = r#"
WITH release_stats AS (
    SELECT
        release,
        MIN(first_seen) AS first_seen,
        SUM(total_count) AS error_count,
        COUNT(DISTINCT fingerprint) AS unique_fingerprints
    FROM bloop.error_aggregates
    WHERE ($1 IS NULL OR project_id = $1)
    GROUP BY release
),
ordered AS (
    SELECT
        *,
        LAG(error_count) OVER (ORDER BY first_seen) AS prev_error_count,
        LAG(release) OVER (ORDER BY first_seen) AS prev_release
    FROM release_stats
),
with_new_fps AS (
    SELECT
        o.*,
        (SELECT COUNT(DISTINCT a.fingerprint)
         FROM bloop.error_aggregates a
         WHERE a.release = o.release
            AND ($1 IS NULL OR a.project_id = $1)
            AND NOT EXISTS (
                SELECT 1 FROM bloop.error_aggregates b
                WHERE b.fingerprint = a.fingerprint
                    AND b.release != o.release
                    AND b.first_seen < a.first_seen
                    AND ($1 IS NULL OR b.project_id = $1)
            )
        ) AS new_fingerprints
    FROM ordered o
)
SELECT
    release,
    first_seen,
    error_count,
    unique_fingerprints,
    new_fingerprints,
    error_count - COALESCE(prev_error_count, 0) AS error_delta,
    (new_fingerprints * 10.0) +
        CASE WHEN COALESCE(prev_error_count, 0) > 0
            THEN ((error_count - prev_error_count) * 100.0 / prev_error_count)
            ELSE 0.0
        END AS impact_score
FROM with_new_fps
ORDER BY first_seen DESC
LIMIT $2
"#;

/// Environment breakdown with percentile hourly rates.
/// Parameters: $1 = since_ms, $2 = project_id (or NULL), $3 = environment (or NULL), $4 = limit
pub const ENVIRONMENTS_SQL: &str = r#"
WITH hourly AS (
    SELECT
        environment,
        hour_bucket,
        SUM(count) AS hourly_count
    FROM bloop.event_counts_hourly
    WHERE hour_bucket >= $1
        AND ($2 IS NULL OR project_id = $2)
        AND ($3 IS NULL OR environment = $3)
    GROUP BY environment, hour_bucket
),
env_stats AS (
    SELECT
        environment,
        SUM(hourly_count) AS total_count,
        PERCENTILE_CONT(0.50) WITHIN GROUP (ORDER BY hourly_count) AS p50_hourly,
        PERCENTILE_CONT(0.90) WITHIN GROUP (ORDER BY hourly_count) AS p90_hourly,
        PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY hourly_count) AS p99_hourly
    FROM hourly
    GROUP BY environment
),
unique_errors AS (
    SELECT
        environment,
        COUNT(DISTINCT fingerprint) AS unique_errors
    FROM bloop.error_aggregates
    WHERE ($2 IS NULL OR project_id = $2)
        AND ($3 IS NULL OR environment = $3)
    GROUP BY environment
),
grand_total AS (
    SELECT SUM(total_count) AS total FROM env_stats
)
SELECT
    e.environment,
    e.total_count,
    COALESCE(u.unique_errors, 0) AS unique_errors,
    CASE WHEN g.total > 0 THEN (e.total_count * 100.0 / g.total) ELSE 0 END AS pct_of_total,
    e.p50_hourly,
    e.p90_hourly,
    e.p99_hourly,
    g.total AS grand_total
FROM env_stats e
LEFT JOIN unique_errors u ON e.environment = u.environment
CROSS JOIN grand_total g
ORDER BY e.total_count DESC
LIMIT $4
"#;
