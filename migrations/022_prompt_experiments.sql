-- Migration 022: Prompt Experiments and A/B Testing

-- Experiments table for tracking A/B tests
CREATE TABLE IF NOT EXISTS prompt_experiments (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    prompt_name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);

-- Experiment variants (different prompt versions being tested)
CREATE TABLE IF NOT EXISTS experiment_variants (
    id TEXT PRIMARY KEY,
    experiment_id TEXT NOT NULL,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    prompt_template TEXT NOT NULL,
    traffic_percentage REAL NOT NULL DEFAULT 50.0,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (experiment_id) REFERENCES prompt_experiments(id) ON DELETE CASCADE
);

-- Indexes for experiment queries
CREATE INDEX IF NOT EXISTS idx_prompt_experiments_project_id ON prompt_experiments(project_id);
CREATE INDEX IF NOT EXISTS idx_prompt_experiments_prompt_name ON prompt_experiments(prompt_name);
CREATE INDEX IF NOT EXISTS idx_prompt_experiments_status ON prompt_experiments(status);
CREATE INDEX IF NOT EXISTS idx_experiment_variants_experiment_id ON experiment_variants(experiment_id);
CREATE INDEX IF NOT EXISTS idx_experiment_variants_version ON experiment_variants(version);

-- Composite index for efficient trace lookups during experiment analysis
CREATE INDEX IF NOT EXISTS idx_llm_traces_prompt_version ON llm_traces(prompt_name, prompt_version, started_at);
