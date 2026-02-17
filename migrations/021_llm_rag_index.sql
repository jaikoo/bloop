-- Speed up RAG / span-type analytics queries
CREATE INDEX IF NOT EXISTS idx_llm_spans_type
    ON llm_spans(project_id, span_type, started_at DESC);
