-- Per-trace scoring: user feedback, relevance, accuracy, etc.
-- Supports upsert via PK (project_id, trace_id, name).

CREATE TABLE IF NOT EXISTS llm_trace_scores (
    project_id  TEXT    NOT NULL,
    trace_id    TEXT    NOT NULL,
    name        TEXT    NOT NULL,
    value       REAL    NOT NULL,
    source      TEXT    NOT NULL DEFAULT 'api',
    comment     TEXT,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (project_id, trace_id, name)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_llm_scores_value
    ON llm_trace_scores(project_id, name, value);
