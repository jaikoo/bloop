-- LLM Tracing tables: traces, spans, hourly usage aggregates, project settings
-- Tables always exist regardless of feature flag; only written to when llm-tracing is enabled.

CREATE TABLE IF NOT EXISTS llm_traces (
    project_id  TEXT    NOT NULL,
    id          TEXT    NOT NULL,
    session_id  TEXT,
    user_id     TEXT,
    name        TEXT    NOT NULL DEFAULT '',
    status      TEXT    NOT NULL DEFAULT 'running',
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    total_tokens    INTEGER NOT NULL DEFAULT 0,
    cost_micros     INTEGER NOT NULL DEFAULT 0,
    input       TEXT,
    output      TEXT,
    metadata    TEXT,
    started_at  INTEGER NOT NULL,
    ended_at    INTEGER,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (project_id, id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_llm_traces_session
    ON llm_traces(project_id, session_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_llm_traces_status
    ON llm_traces(project_id, status, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_llm_traces_started
    ON llm_traces(project_id, started_at DESC);

CREATE TABLE IF NOT EXISTS llm_spans (
    project_id  TEXT    NOT NULL,
    id          TEXT    NOT NULL,
    trace_id    TEXT    NOT NULL,
    parent_span_id TEXT,
    span_type   TEXT    NOT NULL DEFAULT 'generation',
    name        TEXT    NOT NULL DEFAULT '',
    model       TEXT,
    provider    TEXT,
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    total_tokens    INTEGER NOT NULL DEFAULT 0,
    cost_micros     INTEGER NOT NULL DEFAULT 0,
    latency_ms      INTEGER NOT NULL DEFAULT 0,
    time_to_first_token_ms INTEGER,
    status      TEXT    NOT NULL DEFAULT 'ok',
    error_message TEXT,
    input       TEXT,
    output      TEXT,
    metadata    TEXT,
    started_at  INTEGER NOT NULL,
    ended_at    INTEGER,
    PRIMARY KEY (project_id, id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_llm_spans_trace
    ON llm_spans(project_id, trace_id, started_at);
CREATE INDEX IF NOT EXISTS idx_llm_spans_model
    ON llm_spans(project_id, model, started_at DESC);

CREATE TABLE IF NOT EXISTS llm_usage_hourly (
    project_id      TEXT    NOT NULL,
    hour_bucket     INTEGER NOT NULL,
    model           TEXT    NOT NULL DEFAULT '',
    provider        TEXT    NOT NULL DEFAULT '',
    span_count      INTEGER NOT NULL DEFAULT 0,
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    total_tokens    INTEGER NOT NULL DEFAULT 0,
    cost_micros     INTEGER NOT NULL DEFAULT 0,
    error_count     INTEGER NOT NULL DEFAULT 0,
    total_latency_ms INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, hour_bucket, model, provider)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS llm_project_settings (
    project_id          TEXT PRIMARY KEY,
    content_storage     TEXT NOT NULL DEFAULT 'none',
    updated_at          INTEGER NOT NULL
);
