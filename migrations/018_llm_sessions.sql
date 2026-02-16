-- Performance index for session listing filtered by user.
-- Sessions are a query-time grouping over llm_traces, no new tables needed.

CREATE INDEX IF NOT EXISTS idx_llm_traces_session_user
    ON llm_traces(project_id, session_id, user_id, started_at DESC)
    WHERE session_id IS NOT NULL;
