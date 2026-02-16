-- Migration 017: Per-model pricing overrides for server-side cost calculation
CREATE TABLE IF NOT EXISTS llm_pricing_overrides (
    model                 TEXT NOT NULL,
    project_id            TEXT,
    input_cost_per_token  REAL NOT NULL,
    output_cost_per_token REAL NOT NULL,
    provider              TEXT,
    updated_at            INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_pricing_overrides_model_project
    ON llm_pricing_overrides(model, COALESCE(project_id, '__global__'));
