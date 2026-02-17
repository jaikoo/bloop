# CLAUDE.md

## Project Overview

bloop is a self-hosted error observability service written in Rust. It ingests error events from mobile apps and APIs, deduplicates them via fingerprinting, and provides a query API for reviewing errors. An optional LLM tracing feature captures AI/LLM call telemetry (token usage, costs, latency) for monitoring AI-powered applications.

## Build & Test

```bash
# Build (core only)
cargo build

# Build with analytics
cargo build --features analytics

# Build with LLM tracing
cargo build --features llm-tracing

# Build with all optional features
cargo build --features "analytics,llm-tracing"

# Run tests (unit + integration)
cargo test

# Run LLM tracing tests
cargo test --features llm-tracing --test llm_tracing_test

# Run the server
cargo run

# Run with LLM tracing enabled
cargo run --features llm-tracing

# Run with custom config
cargo run -- --config path/to/config.toml
```

## Feature Flags

| Feature | Dependency | Purpose |
|---------|-----------|---------|
| `analytics` | `duckdb` | DuckDB-powered insights: spikes, movers, correlations, release impact, env breakdown |
| `llm-tracing` | `duckdb` | LLM call tracing: token usage, costs, latency, hierarchical traces |

Both features are compile-time gated via `#[cfg(feature = "...")]`. Building without either flag produces a binary with no DuckDB dependency.

## Project Structure

Single crate, module-based. Entry point is `src/main.rs`, library re-exports in `src/lib.rs`.

- `src/ingest/` - HTTP ingestion (auth + handler)
- `src/pipeline/` - Event processing (channel consumer + moka aggregator)
- `src/storage/` - SQLite persistence (pool, migrations, writer, retention)
- `src/query/` - Read API endpoints
- `src/alert/` - Rule evaluation + webhook dispatch (supports error, analytics, and LLM alert types)
- `src/analytics/` - DuckDB analytics queries + cache (feature: `analytics`)
- `src/llm_tracing/` - LLM tracing pipeline, queries, dashboard (feature: `llm-tracing`)
- `src/auth/` - WebAuthn passkeys, sessions, bearer tokens
- `src/project/` - Multi-project CRUD
- `src/sourcemap/` - Source map upload + stack trace deobfuscation
- `src/fingerprint.rs` - Message normalization + xxhash3
- `src/config.rs` - TOML + env var config
- `src/types.rs` - Shared types (event contract, query params, responses)
- `src/error.rs` - Unified error type

### LLM Tracing Module (`src/llm_tracing/`)

| File | Purpose |
|------|---------|
| `mod.rs` | `LlmIngestState`, `LlmQueryState`, module root |
| `types.rs` | `IngestTrace`, `IngestSpan`, `ProcessedTrace`, query response types, microdollar conversion |
| `handler.rs` | POST `/v1/traces`, POST `/v1/traces/batch`, PUT `/v1/traces/{id}` |
| `worker.rs` | Pipeline: channel consumer + batch flush (mirrors `pipeline::worker`) |
| `writer.rs` | SQLite batch writer: traces + spans + hourly upserts in single transaction |
| `query.rs` | DuckDB SQL constants for all query endpoints |
| `query_handler.rs` | GET endpoints: dashboard, metrics, search, prompts, scores, sessions, tools, prompt versions, RAG |
| `settings.rs` | Per-project content storage CRUD + moka cache |
| `alerts.rs` | Timer-based LLM alert evaluator: cost spike, error rate, latency, budget thresholds with cooldown tracking |
| `scores.rs` | Per-trace scoring: POST/GET scores, score summary aggregation |
| `feedback.rs` | Per-trace user feedback: thumbs up/down with comments, summary aggregation |
| `budget.rs` | Per-project monthly cost budgets with daily-rate forecasting |
| `pricing.rs` | LLM model pricing table (per-token costs) |
| `pricing_handler.rs` | Pricing CRUD endpoints |
| `conn.rs` | DuckDB connection wrapper (self-contained, no dependency on analytics feature) |
| `cache.rs` | Query result cache wrapper |

**API Endpoints:**
- Ingest: POST `/v1/traces`, POST `/v1/traces/batch`, PUT `/v1/traces/{id}`
- Query: GET `/v1/llm/dashboard`, `/v1/llm/metrics`, `/v1/llm/traces`, `/v1/llm/traces/{id}`, `/v1/llm/models`, `/v1/llm/usage`, `/v1/llm/errors`, `/v1/llm/settings`
- Search: GET `/v1/llm/search` — full-text search over traces (FTS5)
- Prompts: GET `/v1/llm/prompts` — prompt version tracking with metrics
- Prompt Versions: GET `/v1/llm/prompts/{name}/versions`, `/v1/llm/prompts/{name}/compare?v1=X&v2=Y` — per-version metrics and comparison
- Sessions: GET `/v1/llm/sessions`, `/v1/llm/sessions/{session_id}` — conversation timeline grouping by session_id
- Tools: GET `/v1/llm/tools` — tool/retrieval span analytics (call counts, latency percentiles, error rates)
- RAG: GET `/v1/llm/rag` — RAG pipeline analytics (sources, metrics, relevance from `rag.*` metadata on retrieval spans)
- Scores: POST/GET `/v1/llm/traces/{id}/scores`, GET `/v1/llm/scores/summary` — per-trace quality scoring
- Feedback: POST/GET `/v1/llm/traces/{id}/feedback`, GET `/v1/llm/feedback/summary` — thumbs up/down per trace
- Budget: GET/PUT `/v1/llm/budget` — per-project monthly cost budgets with daily-rate forecasting

## Key Patterns

- **Backpressure**: Channel full -> ACK 200 + drop event (never 429)
- **Batch writes**: Pipeline worker buffers events, flushes on 500 count or 2s timer
- **Single writer**: One pipeline worker task per pipeline, no write contention
- **SQLite WAL**: `PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;`
- **deadpool-sqlite**: Async wrapper, use `.interact(|conn| { ... })` for DB access
- **InteractError**: Does not impl Sync, must `.map_err()` before `?` into `Box<dyn Error + Send + Sync>`
- **Feature gating**: Optional modules use `#[cfg(feature = "...")]` in both `lib.rs` and `main.rs`
- **DuckDB sqlite_scanner**: Analytics/LLM query endpoints attach SQLite read-only via DuckDB for window functions and percentiles
- **Microdollars**: LLM costs stored as integers (1 dollar = 1,000,000 micros) — no float precision issues

## Configuration

`config.toml` in project root. All values overridable via `BLOOP__SECTION__KEY` env vars (double underscore separator).

Webhook URLs via env: `BLOOP_SLACK_WEBHOOK_URL`, `BLOOP_WEBHOOK_URL`.

### LLM Tracing Config

```toml
[llm_tracing]
enabled = true                    # runtime toggle (feature flag must also be compiled in)
channel_capacity = 4096           # bounded MPSC channel size
flush_interval_secs = 2           # time-based flush trigger
flush_batch_size = 200            # count-based flush trigger
max_spans_per_trace = 100         # validation limit
max_batch_size = 50               # max traces per batch POST
default_content_storage = "none"  # none | metadata_only | full
cache_ttl_secs = 30               # query result cache TTL
```

Content storage policies control what gets persisted:
- `none` — prompts/completions stripped at ingest (default, most private)
- `metadata_only` — only metadata JSON kept, no input/output text
- `full` — everything stored (useful for debugging, audit)

### Alert Rules

Alert rule types in `src/alert/rules.rs`:
- **Error alerts:** `error_spike`, `error_new`, `error_rate`
- **Analytics alerts:** `spike`, `mover`, `correlation`
- **LLM alerts:** `llm_cost_spike`, `llm_error_rate`, `llm_latency`, `llm_budget` (with cooldown tracking via migration 013)

## Database

SQLite file at path configured in `config.toml` (default: `bloop.db`). Migrations in `migrations/`, applied automatically on startup via `storage/migrations.rs`.

### Migrations

| Migration | Tables |
|-----------|--------|
| 001 | `raw_events`, `error_aggregates`, `sample_occurrences` |
| 002 | `webauthn_users`, `webauthn_credentials`, `sessions` |
| 003 | Multi-user roles |
| 004 | `projects` (with data migration) |
| 005 | `event_counts_hourly` |
| 006 | `alert_channels` |
| 007 | `sourcemaps` |
| 008 | Index optimization |
| 009 | `bearer_tokens` |
| 010 | Hashed sessions |
| 011 | `retention_settings`, `project_retention` |
| 012 | `llm_traces`, `llm_spans`, `llm_usage_hourly`, `llm_project_settings` |
| 013 | `llm_alert_cooldowns` — alert dedup tracking for LLM metrics |
| 014 | `llm_traces_fts` — FTS5 virtual table for full-text search, filter indexes |
| 015 | `llm_trace_scores` — per-trace quality scores with upsert support |
| 016 | ALTER TABLE `llm_traces` ADD COLUMN `prompt_name`, `prompt_version` |
| 017 | `llm_pricing_overrides` — per-project model pricing overrides |
| 018 | Index on `llm_traces(project_id, session_id, user_id, started_at)` for session queries |
| 019 | `llm_trace_feedback` — per-trace thumbs up/down with upsert by (project, trace, user) |
| 020 | `llm_cost_budgets` — per-project monthly cost budgets with alert threshold |
| 021 | Index on `llm_spans(project_id, span_type, started_at DESC)` for RAG/span-type queries |

Migration 012+ always run (tables exist regardless of feature flag), but tables are only written to when `llm-tracing` feature is compiled in AND `llm_tracing.enabled = true`.

## Testing

Integration tests in `tests/integration_test.rs` spin up a real server on a random port with a temp SQLite database. They test the full ingest -> query round-trip including HMAC auth.

LLM tracing tests in `tests/llm_tracing_test.rs` (feature-gated, needs `--features llm-tracing`). Covers: single trace ingest, batch ingest, validation (ID length, span count), HMAC auth enforcement, trace update, microdollar conversion, sessions, tools, feedback (post/get/upsert/validation/summary), prompt versions, and cost budgets.

Security tests in `tests/security_test.rs` cover HMAC validation, bearer tokens, and session handling.

Unit tests in `src/fingerprint.rs` cover normalization and dedup.

### SDK Integration Tests

Cross-SDK integration tests in `tests/sdk-integration/`:
- **`run.sh`** — Local test runner: builds bloop with `llm-tracing`, starts server on random port, runs SDK tests
- **`docker-compose.yml`** — Docker-based runner for CI environments
- Tests verify: trace ingestion, span hierarchy, cost tracking, auto-instrumentation (TypeScript/Python)

Covers all 7 SDKs: TypeScript, Python, Ruby, Swift, Kotlin, React Native, Rust.

## SDKs

bloop has 7 official SDKs maintained in separate GitHub repositories:

| Language | Repo | LLM Tracing |
|----------|------|-------------|
| TypeScript | `jaikoo/bloop-js` | ✓ Manual + auto-instrumentation (`wrapOpenAI`, `wrapAnthropic`) |
| Python | `jaikoo/bloop-python` | ✓ Manual + auto-instrumentation (`wrapOpenAI`, `wrapAnthropic`) |
| Ruby | `jaikoo/bloop-ruby` | ✓ Manual API |
| Swift | `jaikoo/bloop-swift` | ✓ Manual API |
| Kotlin | `jaikoo/bloop-kotlin` | ✓ Manual API |
| React Native | `jaikoo/bloop-react-native` | ✓ Manual API |
| Rust | `jaikoo/bloop-rust` | ✓ Manual API |

**LLM Tracing Pattern:**
```
client.startTrace() → trace.startSpan() → span.end() → trace.end() → buffer → POST /v1/traces/batch
```

Auto-instrumentation (TypeScript/Python) wraps OpenAI/Anthropic SDK clients to automatically capture prompts, completions, token usage, and costs without manual span management.

## Git

- Do not add `Co-Authored-By` lines to commits

## Common Gotchas

- `moka` requires both `sync` and `future` features
- `rusqlite` and `deadpool-sqlite` versions must align on the same `libsqlite3-sys`
- Fingerprint normalization runs regex replacements in order: UUIDs -> IPs -> numbers -> lowercase
- The `\d+` number regex matches all digit sequences (not word-bounded) to catch things like `5000ms`
- LLM tracing uses microdollars (1 dollar = 1,000,000 micros) for cost storage — no float precision issues
- LLM tracing migration 012 always runs (tables exist regardless), only written to when feature+config enabled
- `llm-tracing` feature depends on `duckdb` (same as `analytics`), query endpoints use DuckDB via sqlite_scanner
- The `llm-tracing` module has its own `conn.rs`/`cache.rs` — does NOT depend on `analytics` feature being enabled
- When adding new fields to `AppConfig`, also update `tests/security_test.rs` which constructs it directly
- Two state patterns for LLM handlers: `Arc<LlmQueryState>` (DuckDB) for analytical queries vs `Arc<Pool>` (direct SQLite) for mutations (feedback, budget, scores)
- `chrono::Datelike` trait must be imported (`use chrono::Datelike;`) when using `.with_day()` or `.day()` on date types
