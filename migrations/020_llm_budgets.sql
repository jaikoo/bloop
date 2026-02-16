-- Per-project monthly cost budgets with alert threshold.

CREATE TABLE IF NOT EXISTS llm_cost_budgets (
    project_id            TEXT    PRIMARY KEY,
    monthly_budget_micros INTEGER NOT NULL DEFAULT 0,
    alert_threshold_pct   INTEGER NOT NULL DEFAULT 80,
    updated_at            INTEGER NOT NULL
);
