-- Per-trace user feedback: thumbs up/down with optional comment.
-- Binary (1/-1) vs scores which are continuous (0.0-1.0).

CREATE TABLE IF NOT EXISTS llm_trace_feedback (
    project_id  TEXT    NOT NULL,
    trace_id    TEXT    NOT NULL,
    user_id     TEXT    NOT NULL DEFAULT 'anonymous',
    value       INTEGER NOT NULL,  -- 1 (positive) or -1 (negative)
    comment     TEXT,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (project_id, trace_id, user_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_llm_feedback_value
    ON llm_trace_feedback(project_id, value, created_at DESC);
