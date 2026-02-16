-- Index optimization: add missing project-scoped indexes, drop redundant ones

-- 1. raw_events: project + timestamp for stats dashboard (COUNT WHERE project_id AND timestamp)
CREATE INDEX IF NOT EXISTS idx_raw_project_timestamp
    ON raw_events(project_id, timestamp);

-- 2. sample_occurrences: project-aware fingerprint lookups (write-path COUNT + DELETE)
DROP INDEX IF EXISTS idx_samples_fp_ts;
CREATE INDEX IF NOT EXISTS idx_samples_project_fp_ts
    ON sample_occurrences(project_id, fingerprint, captured_at DESC);

-- 3. event_counts_hourly: per-fingerprint trend queries
CREATE INDEX IF NOT EXISTS idx_hourly_fp_project_bucket
    ON event_counts_hourly(fingerprint, project_id, hour_bucket);

-- 4. alert_rules: enabled rule lookups by type and project
CREATE INDEX IF NOT EXISTS idx_alert_rules_enabled_type
    ON alert_rules(enabled, rule_type, project_id)
    WHERE enabled = 1;

-- 5. event_counts_hourly: retention cleanup by hour_bucket
CREATE INDEX IF NOT EXISTS idx_hourly_bucket
    ON event_counts_hourly(hour_bucket);

-- 6. Drop redundant pre-project indexes (superseded by project-scoped versions from migration 004)
DROP INDEX IF EXISTS idx_agg_release_count;
DROP INDEX IF EXISTS idx_agg_last_seen;
DROP INDEX IF EXISTS idx_agg_unresolved;

-- 7. Drop unused index (no query reads raw_events by release+timestamp)
DROP INDEX IF EXISTS idx_raw_release_ts;
