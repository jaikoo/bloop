-- Projects table
CREATE TABLE IF NOT EXISTS projects (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    slug        TEXT NOT NULL UNIQUE,
    api_key     TEXT NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL,
    created_by  TEXT REFERENCES webauthn_users(id)
);
CREATE INDEX IF NOT EXISTS idx_projects_api_key ON projects(api_key);

-- Add project_id to existing tables
ALTER TABLE raw_events ADD COLUMN project_id TEXT REFERENCES projects(id);
ALTER TABLE sample_occurrences ADD COLUMN project_id TEXT REFERENCES projects(id);
ALTER TABLE alert_rules ADD COLUMN project_id TEXT REFERENCES projects(id);

-- Recreate error_aggregates with project_id in PK
CREATE TABLE IF NOT EXISTS error_aggregates_v2 (
    project_id         TEXT NOT NULL REFERENCES projects(id),
    fingerprint        TEXT NOT NULL,
    release            TEXT NOT NULL,
    environment        TEXT NOT NULL,
    total_count        INTEGER NOT NULL DEFAULT 0,
    first_seen         INTEGER NOT NULL,
    last_seen          INTEGER NOT NULL,
    error_type         TEXT NOT NULL,
    message            TEXT NOT NULL,
    source             TEXT NOT NULL,
    route_or_procedure TEXT,
    screen             TEXT,
    status             TEXT NOT NULL DEFAULT 'unresolved',
    PRIMARY KEY (project_id, fingerprint, release, environment)
) WITHOUT ROWID;

-- Recreate alert_cooldowns with project_id
CREATE TABLE IF NOT EXISTS alert_cooldowns_v2 (
    project_id   TEXT NOT NULL REFERENCES projects(id),
    rule_id      INTEGER NOT NULL,
    fingerprint  TEXT NOT NULL,
    last_fired   INTEGER NOT NULL,
    PRIMARY KEY (project_id, rule_id, fingerprint)
) WITHOUT ROWID;
