# Architecture

## Overview

bloop is a single-binary Rust service that ingests error events via HTTP, deduplicates them using fingerprinting, aggregates counts in-memory, and flushes to SQLite in batches. Alerting evaluates rules on each new fingerprint. An optional LLM tracing pipeline captures AI/LLM call telemetry (traces, spans, token usage, costs) through a separate channel and worker.

```
SDK (iOS/API/LLM)
    |
    v
+----------------------------------+
|  HTTP Layer (axum)               |
|  +- HMAC Auth middleware         |
|  +- Size limit (32KB)           |
|  +- Schema validation (serde)   |
+---+-------------+---------------+
    |             |
    | errors      | LLM traces (#[cfg])
    v             v
+--------+   +-----------+
| Error  |   | LLM Trace |
| Channel|   | Channel   |
| (8192) |   | (4096)    |
+---+----+   +-----+-----+
    |              |
    v              v
+--------+   +-----------+
| Error  |   | LLM Trace |
| Worker |   | Worker    |
| (batch)|   | (batch)   |
+---+----+   +-----+-----+
    |              |
    v              v
+----------------------------------+
|  SQLite Writer                   |
|  +- BEGIN IMMEDIATE              |
|  +- INSERT raw_events (batch)    |
|  +- UPSERT error_aggregates     |
|  +- INSERT/prune samples        |
|  +- INSERT llm_traces/spans     |
|  +- UPSERT llm_usage_hourly     |
|  +- COMMIT                       |
+----------------------------------+
         |
         v
+----------------------------------+
|  Query Layer                     |
|  +- SQLite direct (error CRUD)   |
|  +- DuckDB sqlite_scanner       |
|     (analytics, LLM analytics)   |
+----------------------------------+
```

## Module Structure

```
src/
+- main.rs              # Entry point, router, graceful shutdown
+- lib.rs               # Re-exports for integration tests
+- config.rs            # TOML + env var config
+- error.rs             # AppError -> axum JSON responses
+- types.rs             # Event contract, query/response types
+- fingerprint.rs       # Message normalization + xxhash3
+- ingest/
|  +- auth.rs           # HMAC-SHA256 middleware, ProjectKeyCache
|  +- handler.rs        # POST /v1/ingest, /v1/ingest/batch
+- pipeline/
|  +- aggregator.rs     # Moka cache for in-memory counts
|  +- worker.rs         # Channel consumer, flush logic
+- storage/
|  +- sqlite.rs         # Pool creation, WAL pragmas
|  +- migrations.rs     # Schema versioning (12 migrations)
|  +- writer.rs         # Batch INSERT + UPSERT (errors)
|  +- retention.rs      # Background pruning loop (errors + LLM data)
|  +- retention_api.rs  # Admin retention settings endpoints
+- query/
|  +- handler.rs        # All GET endpoints for errors
+- alert/
|  +- rules.rs          # Rule evaluation, CRUD, cooldowns
|  +- dispatch.rs       # Slack/webhook/email sender
+- auth/
|  +- webauthn.rs       # Passkey registration/login
|  +- session.rs        # Session middleware + cleanup
|  +- bearer.rs         # API token auth middleware
|  +- token_routes.rs   # Token CRUD
+- project/
|  +- mod.rs            # Project CRUD + key rotation
+- sourcemap/
|  +- mod.rs            # Source map upload/deobfuscation
+- analytics/           # [feature = "analytics"]
|  +- mod.rs            # AnalyticsState
|  +- conn.rs           # DuckDB connection wrapper
|  +- cache.rs          # Moka query cache
|  +- handler.rs        # 5 analytics endpoints
|  +- queries.rs        # DuckDB SQL constants
|  +- types.rs          # Query param + response types
+- llm_tracing/         # [feature = "llm-tracing"]
   +- mod.rs            # LlmIngestState, LlmQueryState
   +- types.rs          # Ingest, processed, and response types
   +- handler.rs        # POST /v1/traces, batch, PUT update
   +- worker.rs         # Channel consumer, batch flush
   +- writer.rs         # SQLite batch writer (traces+spans+hourly)
   +- query.rs          # DuckDB SQL constants
   +- query_handler.rs  # 8 query endpoints
   +- settings.rs       # Per-project content storage + cache
   +- conn.rs           # DuckDB connection (self-contained)
   +- cache.rs          # Query result cache
```

## Data Flow

### Error Ingest Path (hot)

1. Request arrives at `POST /v1/ingest`
2. HMAC middleware verifies `X-Signature` header against body
3. Handler deserializes + validates payload against size limits
4. Fingerprint computed: normalize message (strip UUIDs, IPs, numbers) + xxhash3
5. `ProcessedEvent` sent to bounded MPSC channel via `try_send`
6. Response: `200 {"status": "accepted"}` (always, even if channel full)

### Error Pipeline (background)

1. Worker receives events from channel
2. Updates moka cache (in-memory aggregate counts)
3. If new fingerprint: sends `NewFingerprintEvent` to alert channel
4. Buffers events until flush trigger (500 events OR 2-second timer)
5. Flush: single SQLite transaction with batch INSERT + UPSERT

### LLM Trace Ingest Path (feature-gated)

1. Request arrives at `POST /v1/traces` or `POST /v1/traces/batch`
2. HMAC middleware verifies `X-Signature` header
3. Handler validates (trace ID length <= 128, spans <= 100, batch <= 50)
4. Content stripped per project policy: `none` strips input/output/metadata, `metadata_only` keeps metadata, `full` keeps everything
5. Dollar costs converted to microdollars (integer): `$0.0025 -> 2500 micros`
6. Span tokens aggregated to trace-level totals
7. `ProcessedTrace` sent to separate bounded MPSC channel via `try_send`
8. Response: `200 {"status": "accepted"}`

### LLM Trace Pipeline (background, feature-gated)

1. Separate worker task receives traces from LLM channel
2. Buffers until flush trigger (200 traces OR 2-second timer)
3. Flush: single SQLite transaction:
   - `INSERT OR REPLACE` into `llm_traces`
   - `INSERT OR REPLACE` into `llm_spans`
   - `UPSERT` into `llm_usage_hourly` (pre-aggregated by model/provider/hour)

### Query Path (cold)

1. Request arrives at `GET /v1/errors` or `GET /v1/llm/*`
2. Error queries: handler builds SQL with dynamic WHERE clauses, executes via deadpool-sqlite
3. LLM/Analytics queries: DuckDB attaches SQLite read-only via sqlite_scanner, runs window functions and percentiles
4. Returns JSON response

## Key Design Decisions

### Backpressure: ACK + Drop

When the MPSC channel is full (8192 for errors, 4096 for LLM traces), the ingest handler:
- Returns `200` (ACK)
- Drops the event silently
- Does NOT return `429`

Rationale: returning 429 causes SDK retry storms that amplify load. Better to lose a few events than to cascade failure.

### Single Pipeline Worker (per pipeline)

One tokio task consumes each channel. This means:
- No contention on writes (only one writer per pipeline)
- Batch size is naturally bounded
- SQLite writer only called from one task per pipeline (no WAL contention)

The error pipeline and LLM pipeline have independent channels and workers, so they don't block each other.

### Separate LLM Pipeline

The LLM tracing pipeline is fully independent from the error pipeline:
- Separate MPSC channel (4096 capacity)
- Separate worker task with independent flush cadence (200 batch / 2s timer)
- Separate tables (`llm_traces`, `llm_spans`, `llm_usage_hourly`)
- Compile-time gated (`#[cfg(feature = "llm-tracing")]`)
- Runtime toggle (`llm_tracing.enabled` in config)

This ensures LLM tracing workloads can't interfere with core error tracking.

### Microdollar Cost Storage

LLM costs are stored as integers in microdollars (1 dollar = 1,000,000 micros):
- `$0.0025` -> `2500` micros
- No floating-point precision issues in aggregation
- Conversion happens once at ingest: `(dollars * 1_000_000).round() as i64`
- Dashboard formats back to dollars for display

### Content Stripping at Ingest

LLM prompts and completions can contain sensitive data. Content storage policy is per-project:
- **`none`** (default): `input`, `output`, `metadata` fields set to NULL before write
- **`metadata_only`**: only `metadata` JSON kept
- **`full`**: everything stored

Stripping happens in `handler.rs` before the trace enters the channel, so sensitive data never touches the disk.

### SQLite WAL Mode

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
```

WAL allows concurrent reads while the writer flushes. `synchronous = NORMAL` trades a tiny crash-recovery window for 2-3x write throughput.

### Fingerprint Algorithm

```
Input: "{source}:{error_type}:{route}:{normalized_message}:{top_stack_frame}"
Hash:  xxh3_64 -> 16-char hex string
```

Normalization strips variable parts (UUIDs, IPs, numbers) so that `"Timeout after 5000ms"` and `"Timeout after 3000ms"` produce the same fingerprint.

### Sample Reservoir

For each fingerprint, we keep the N most recent sample occurrences (default 5). On each insert, if count exceeds N, the oldest is pruned. This gives you concrete examples of each error without unbounded storage.

### DuckDB for Analytics

Both the analytics and LLM tracing query endpoints use DuckDB (in-memory) with the sqlite_scanner extension to read SQLite tables. This provides:
- Window functions (`PERCENTILE_CONT`, `STDDEV_SAMP`, `CORR`)
- `FULL OUTER JOIN` for comparing time periods
- Read-only access (no write contention)

Each feature has its own `DuckDbConn` wrapper so `analytics` and `llm-tracing` can be enabled independently.

## Database Schema

### Core Error Tables

| Table | Purpose | Retention |
|-------|---------|-----------|
| `raw_events` | Full event data | Configurable (default 7 days) |
| `error_aggregates` | Counts per fingerprint+release+env | Forever |
| `sample_occurrences` | N recent examples per fingerprint | Rolling (default 5) |
| `event_counts_hourly` | Pre-aggregated hourly counts | Configurable (default 90 days) |

### Alert Tables

| Table | Purpose |
|-------|---------|
| `alert_rules` | Alert rule configuration |
| `alert_channels` | Notification channels (Slack, webhook, email) |
| `alert_cooldowns` | Dedup: last-fired timestamp per rule+fingerprint |

### Auth Tables

| Table | Purpose |
|-------|---------|
| `webauthn_users` | User accounts |
| `webauthn_credentials` | Passkey credentials |
| `sessions` | Hashed session tokens |
| `bearer_tokens` | Scoped API tokens |

### Project Tables

| Table | Purpose |
|-------|---------|
| `projects` | Project metadata + API keys |
| `sourcemaps` | Uploaded source maps |
| `retention_settings` | Global retention config |
| `project_retention` | Per-project retention overrides |

### LLM Tracing Tables (migration 012)

| Table | PK | Purpose |
|-------|-----|---------|
| `llm_traces` | `(project_id, id)` WITHOUT ROWID | Top-level trace grouping |
| `llm_spans` | `(project_id, id)` WITHOUT ROWID | Individual operations (generation, tool, retrieval, custom) |
| `llm_usage_hourly` | `(project_id, hour_bucket, model, provider)` WITHOUT ROWID | Pre-aggregated hourly stats |
| `llm_project_settings` | `project_id` | Per-project content storage policy |

LLM tables always exist (migration runs unconditionally) but are only written to when the feature is compiled in and enabled.

### Aggregate UPSERT (hot path)

```sql
INSERT INTO error_aggregates (...)
VALUES (...)
ON CONFLICT (fingerprint, release, environment) DO UPDATE SET
    total_count = total_count + 1,
    last_seen   = excluded.last_seen,
    status      = CASE WHEN status = 'resolved' THEN 'unresolved' ELSE status END;
```

The `CASE` on status means a resolved error automatically re-opens if it recurs.

### LLM Hourly UPSERT

```sql
INSERT INTO llm_usage_hourly (...)
VALUES (...)
ON CONFLICT (project_id, hour_bucket, model, provider) DO UPDATE SET
    span_count = span_count + excluded.span_count,
    input_tokens = input_tokens + excluded.input_tokens,
    ...
```

Pre-aggregated at write time to make dashboard queries fast without scanning raw spans.

## Concurrency Model

```
Main thread
+- axum server (accepts connections)
+- Error pipeline worker task (channel consumer)
+- LLM tracing worker task (channel consumer) [feature-gated]
+- Alert evaluator task (new-fingerprint listener)
+- Retention task (hourly prune loop — covers errors + LLM data)
+- Session cleanup task (expired session removal)
```

All async, single-process. SQLite access goes through `deadpool-sqlite` which dispatches to a blocking threadpool via `spawn_blocking`. DuckDB queries also use `spawn_blocking` via a `Mutex<Connection>`.

## Graceful Shutdown

1. `SIGTERM` or `Ctrl+C` received
2. axum stops accepting new connections, drains in-flight requests
3. Error channel sender is dropped -> error worker receives channel-closed, drains buffer
4. LLM channel is dropped when ingest state drops -> LLM worker drains buffer
5. Main waits for error worker with 10-second timeout

## Feature Flag Architecture

```
Cargo.toml:
  analytics = ["duckdb"]       # DuckDB analytics queries
  llm-tracing = ["duckdb"]     # LLM tracing pipeline + queries

Compile-time gating:
  lib.rs:    #[cfg(feature = "...")] pub mod ...;
  main.rs:   #[cfg(feature = "...")] mod ...;
             #[cfg(feature = "...")] { channel, worker, state, routes }
  error.rs:  #[cfg(feature = "...")] variant

Runtime gating:
  config.toml: [analytics] enabled = true/false
               [llm_tracing] enabled = true/false
```

Both levels must be active for a feature to work. Building without a feature flag excludes all related code from the binary.
