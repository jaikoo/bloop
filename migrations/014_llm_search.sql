-- Full-text search on trace names and span error messages.
-- Contentless FTS5 table populated during ingest.

CREATE VIRTUAL TABLE IF NOT EXISTS llm_traces_fts USING fts5(
    name,
    error_messages,
    content='',
    contentless_delete=1
);

-- Indexes on commonly filtered fields
CREATE INDEX IF NOT EXISTS idx_llm_traces_user
    ON llm_traces(project_id, user_id) WHERE user_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_llm_spans_model_status
    ON llm_spans(project_id, model, status);
