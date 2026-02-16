-- Raw events (short retention: 7 days, pruned by background task)
CREATE TABLE IF NOT EXISTS raw_events (
    id                 INTEGER PRIMARY KEY,
    timestamp          INTEGER NOT NULL,
    source             TEXT NOT NULL,
    environment        TEXT NOT NULL,
    release            TEXT NOT NULL,
    app_version        TEXT,
    build_number       TEXT,
    route_or_procedure TEXT,
    screen             TEXT,
    error_type         TEXT NOT NULL,
    message            TEXT NOT NULL,
    stack              TEXT,
    http_status        INTEGER,
    request_id         TEXT,
    user_id_hash       TEXT,
    device_id_hash     TEXT,
    fingerprint        TEXT NOT NULL,
    metadata           TEXT,
    received_at        INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_raw_timestamp ON raw_events(timestamp);
CREATE INDEX IF NOT EXISTS idx_raw_fingerprint ON raw_events(fingerprint);
CREATE INDEX IF NOT EXISTS idx_raw_release_ts ON raw_events(release, timestamp);

-- Aggregates (long retention, UPSERT target)
CREATE TABLE IF NOT EXISTS error_aggregates (
    fingerprint        TEXT NOT NULL,
    release            TEXT NOT NULL,
    environment        TEXT NOT NULL,
    total_count        INTEGER NOT NULL DEFAULT 0,
    first_seen         INTEGER NOT NULL,
    last_seen          INTEGER NOT NULL,
    error_type         TEXT NOT NULL,
    message            TEXT NOT NULL,
    source             TEXT NOT NULL,
    route_or_procedure TEXT,
    screen             TEXT,
    status             TEXT NOT NULL DEFAULT 'unresolved',
    PRIMARY KEY (fingerprint, release, environment)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_agg_release_count
    ON error_aggregates(release, total_count DESC);
CREATE INDEX IF NOT EXISTS idx_agg_route_count
    ON error_aggregates(route_or_procedure, total_count DESC)
    WHERE route_or_procedure IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_agg_last_seen
    ON error_aggregates(last_seen DESC);
CREATE INDEX IF NOT EXISTS idx_agg_unresolved
    ON error_aggregates(status, last_seen DESC)
    WHERE status = 'unresolved';

-- Sample occurrences (keep N=5 per fingerprint)
CREATE TABLE IF NOT EXISTS sample_occurrences (
    id                 INTEGER PRIMARY KEY,
    fingerprint        TEXT NOT NULL,
    captured_at        INTEGER NOT NULL,
    source             TEXT NOT NULL,
    environment        TEXT NOT NULL,
    release            TEXT NOT NULL,
    error_type         TEXT NOT NULL,
    message            TEXT NOT NULL,
    stack              TEXT,
    request_id         TEXT,
    metadata           TEXT
);

CREATE INDEX IF NOT EXISTS idx_samples_fp_ts
    ON sample_occurrences(fingerprint, captured_at DESC);

-- Alert rules
CREATE TABLE IF NOT EXISTS alert_rules (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    rule_type   TEXT NOT NULL,
    config      TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- Alert cooldowns (deduplication)
CREATE TABLE IF NOT EXISTS alert_cooldowns (
    rule_id      INTEGER NOT NULL,
    fingerprint  TEXT NOT NULL,
    last_fired   INTEGER NOT NULL,
    PRIMARY KEY (rule_id, fingerprint)
) WITHOUT ROWID;
