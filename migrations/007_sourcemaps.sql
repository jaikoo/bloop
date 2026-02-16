-- Source map storage for stack trace deobfuscation
CREATE TABLE IF NOT EXISTS source_maps (
    id          INTEGER PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id),
    release     TEXT NOT NULL,
    filename    TEXT NOT NULL,
    map_data    BLOB NOT NULL,
    uploaded_at INTEGER NOT NULL,
    uploaded_by TEXT REFERENCES webauthn_users(id),
    UNIQUE(project_id, release, filename)
);
CREATE INDEX IF NOT EXISTS idx_sourcemaps_project_release
    ON source_maps(project_id, release);
