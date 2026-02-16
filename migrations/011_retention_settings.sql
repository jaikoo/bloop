-- Configurable retention settings
CREATE TABLE IF NOT EXISTS retention_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Per-project retention overrides
CREATE TABLE IF NOT EXISTS project_retention (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    raw_events_days INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
