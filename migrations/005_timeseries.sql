-- Hourly event counts for trend charts
CREATE TABLE IF NOT EXISTS event_counts_hourly (
    project_id    TEXT NOT NULL,
    fingerprint   TEXT NOT NULL,
    hour_bucket   INTEGER NOT NULL,
    environment   TEXT NOT NULL,
    source        TEXT NOT NULL,
    count         INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, fingerprint, hour_bucket, environment, source)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_hourly_project_hour
    ON event_counts_hourly(project_id, hour_bucket DESC);

-- Expand sample_occurrences with device/context fields
ALTER TABLE sample_occurrences ADD COLUMN user_id_hash TEXT;
ALTER TABLE sample_occurrences ADD COLUMN device_id_hash TEXT;
ALTER TABLE sample_occurrences ADD COLUMN app_version TEXT;
ALTER TABLE sample_occurrences ADD COLUMN build_number TEXT;
ALTER TABLE sample_occurrences ADD COLUMN screen TEXT;
ALTER TABLE sample_occurrences ADD COLUMN http_status INTEGER;
ALTER TABLE sample_occurrences ADD COLUMN route_or_procedure TEXT;

-- Status change audit trail
CREATE TABLE IF NOT EXISTS status_changes (
    id            INTEGER PRIMARY KEY,
    project_id    TEXT NOT NULL,
    fingerprint   TEXT NOT NULL,
    old_status    TEXT NOT NULL,
    new_status    TEXT NOT NULL,
    changed_by    TEXT REFERENCES webauthn_users(id),
    changed_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_status_changes_fp
    ON status_changes(project_id, fingerprint, changed_at DESC);
