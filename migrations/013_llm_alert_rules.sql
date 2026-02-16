-- LLM alert cooldowns: keyed by metric (cost_hourly, error_rate, latency_p99:model)
-- rather than by fingerprint like error alert cooldowns.

CREATE TABLE IF NOT EXISTS llm_alert_cooldowns (
    project_id TEXT    NOT NULL,
    rule_id    INTEGER NOT NULL,
    metric_key TEXT    NOT NULL,
    last_fired INTEGER NOT NULL,
    PRIMARY KEY (project_id, rule_id, metric_key)
) WITHOUT ROWID;
