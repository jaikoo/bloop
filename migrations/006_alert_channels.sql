-- Alert notification channels per rule
CREATE TABLE IF NOT EXISTS alert_channels (
    id           INTEGER PRIMARY KEY,
    rule_id      INTEGER NOT NULL REFERENCES alert_rules(id) ON DELETE CASCADE,
    channel_type TEXT NOT NULL,  -- 'slack', 'webhook', 'email'
    config       TEXT NOT NULL,  -- JSON: {"webhook_url":"..."} or {"to":"..."}
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_alert_channels_rule ON alert_channels(rule_id);

-- Enrich alert_rules with description and filters
ALTER TABLE alert_rules ADD COLUMN description TEXT;
ALTER TABLE alert_rules ADD COLUMN environment_filter TEXT;
ALTER TABLE alert_rules ADD COLUMN source_filter TEXT;
