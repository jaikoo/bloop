# bloop

Self-hosted error observability service for mobile and API runtime error tracking. A low-cost Sentry alternative written in Rust, designed to run on minimal hardware (single VPS, 1-2 cores, 1-2GB RAM).

## Why

Born from a real incident: an iOS dashboard regression (missing imports in `dashboard.ts`) that should have been surfaced within 60 seconds of deploy. Existing solutions are expensive or overbuilt. bloop gives you the 80% of Sentry you actually need at 1% of the cost.

## Features

- **Ingest** - HMAC-authenticated POST endpoint for single and batch error submission
- **Fingerprinting** - Automatic error deduplication via message normalization + xxhash3
- **Pipeline** - Bounded MPSC channel with backpressure, batch + time-based flush to SQLite
- **Query API** - Paginated, filterable error listing with release/route/status filters
- **Alerting** - New-issue, threshold, and spike detection rules with Slack, webhook, and email dispatch
- **Retention** - Configurable per-project data retention with dashboard controls and on-demand purge
- **Multi-Project** - Isolated projects with scoped API keys, alerts, and source maps
- **Source Maps** - Upload and deobfuscate JavaScript stack traces
- **Analytics** - Optional DuckDB-powered insights: spikes, top movers, correlations, release impact
- **LLM Tracing** - Optional LLM/AI call monitoring: token usage, costs, latency, hierarchical traces
- **LLM Proxy** - Zero-instrumentation tracing via reverse proxy (OpenAI, Anthropic support)
- **LangChain Export** - Export traces as LangSmith format, import OTLP traces
- **Prompt A/B Testing** - Compare prompt variants with statistical analysis
- **Passkey Auth** - WebAuthn-based dashboard authentication with admin/user roles
- **API Tokens** - Scoped bearer tokens for CI pipelines, AI agents, and scripts
- **SDKs** - First-party SDKs for TypeScript, React Native, Swift, Kotlin, Python, and Ruby
- **Graceful Shutdown** - SIGTERM drains in-flight requests and flushes buffered events

## Quick Start

```bash
# Build (core only)
cargo build --release

# Build with all features
cargo build --release --features "analytics,llm-tracing"

# Run (uses config.toml in current directory)
./target/release/bloop

# Or with custom config
./target/release/bloop --config /path/to/config.toml
```

The server starts on `0.0.0.0:5332` by default. Override with environment variables:

```bash
BLOOP__SERVER__PORT=8080 ./target/release/bloop
```

## Sending Events

### Single event

```bash
SECRET="change-me-in-production"
BODY='{"timestamp":1700000000000,"source":"api","environment":"prod","release":"1.0.0","error_type":"TypeError","message":"Cannot read property id of undefined","route_or_procedure":"/api/users"}'
SIG=$(echo -n "$BODY" | openssl dgst -sha256 -hmac "$SECRET" | awk '{print $2}')

curl -X POST http://localhost:5332/v1/ingest \
  -H "Content-Type: application/json" \
  -H "X-Signature: $SIG" \
  -d "$BODY"
```

### Batch

```bash
BODY='{"events":[...]}'
SIG=$(echo -n "$BODY" | openssl dgst -sha256 -hmac "$SECRET" | awk '{print $2}')

curl -X POST http://localhost:5332/v1/ingest/batch \
  -H "Content-Type: application/json" \
  -H "X-Signature: $SIG" \
  -d "$BODY"
```

### LLM Trace

```bash
BODY='{"id":"trace-001","name":"chat-completion","status":"completed","spans":[{"span_type":"generation","model":"gpt-4o","provider":"openai","input_tokens":100,"output_tokens":50,"cost":0.0025,"latency_ms":1200,"time_to_first_token_ms":300,"status":"ok"}]}'
SIG=$(echo -n "$BODY" | openssl dgst -sha256 -hmac "$SECRET" | awk '{print $2}')

curl -X POST http://localhost:5332/v1/traces \
  -H "Content-Type: application/json" \
  -H "X-Signature: $SIG" \
  -d "$BODY"
```

## API Endpoints

### Error Tracking

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/health` | None | Health check |
| `POST` | `/v1/ingest` | HMAC | Submit single error event |
| `POST` | `/v1/ingest/batch` | HMAC | Submit batch (max 50) |
| `GET` | `/v1/errors` | Bearer/Session | List errors (paginated, filterable) |
| `GET` | `/v1/errors/:fingerprint` | Bearer/Session | Error detail + samples |
| `GET` | `/v1/errors/:fingerprint/occurrences` | Bearer/Session | Raw events |
| `POST` | `/v1/errors/:fingerprint/resolve` | Bearer/Session | Mark resolved |
| `POST` | `/v1/errors/:fingerprint/ignore` | Bearer/Session | Mark ignored |
| `POST` | `/v1/errors/:fingerprint/mute` | Bearer/Session | Mute (suppress alerts) |
| `POST` | `/v1/errors/:fingerprint/unresolve` | Bearer/Session | Unresolve |
| `GET` | `/v1/errors/:fingerprint/trend` | Bearer/Session | Per-error hourly counts |
| `GET` | `/v1/errors/:fingerprint/history` | Bearer/Session | Status audit trail |
| `GET` | `/v1/releases/:release/errors` | Bearer/Session | Errors for a release |
| `GET` | `/v1/trends` | Bearer/Session | Global hourly counts |
| `GET` | `/v1/stats` | Bearer/Session | Overview stats |

### Alerting

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/v1/alerts` | Bearer/Session | List alert rules |
| `POST` | `/v1/alerts` | Bearer/Session | Create alert rule |
| `PUT` | `/v1/alerts/:id` | Bearer/Session | Update alert rule |
| `DELETE` | `/v1/alerts/:id` | Bearer/Session | Delete alert rule |
| `POST` | `/v1/alerts/:id/channels` | Bearer/Session | Add notification channel |
| `POST` | `/v1/alerts/:id/test` | Bearer/Session | Send test notification |

### Projects & Auth

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/v1/projects` | Session | List projects |
| `POST` | `/v1/projects` | Session | Create project |
| `GET` | `/v1/projects/:slug` | Session | Project details |
| `POST` | `/v1/projects/:slug/sourcemaps` | Bearer/Session | Upload source map |
| `POST` | `/v1/tokens` | Session | Create API token |
| `GET` | `/v1/tokens` | Session | List tokens |
| `DELETE` | `/v1/tokens/:id` | Session | Revoke token |
| `PUT` | `/v1/admin/users/:id/role` | Admin Session | Promote/demote user |
| `GET` | `/v1/admin/retention` | Admin Session | Get retention config |
| `PUT` | `/v1/admin/retention` | Admin Session | Update retention config |
| `POST` | `/v1/admin/retention/purge` | Admin Session | Trigger immediate purge |

### Analytics (feature: `analytics`)

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/v1/analytics/spikes` | Bearer/Session | Spike detection (z-score) |
| `GET` | `/v1/analytics/movers` | Bearer/Session | Top movers |
| `GET` | `/v1/analytics/correlations` | Bearer/Session | Correlated error pairs |
| `GET` | `/v1/analytics/releases` | Bearer/Session | Release impact scoring |
| `GET` | `/v1/analytics/environments` | Bearer/Session | Environment breakdown |

### LLM Tracing (feature: `llm-tracing`)

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `POST` | `/v1/traces` | HMAC | Submit single LLM trace with spans |
| `POST` | `/v1/traces/batch` | HMAC | Submit batch of traces (max 50) |
| `PUT` | `/v1/traces/:id` | HMAC | Update a running trace |
| `POST` | `/v1/proxy/{provider}/{*path}` | Bearer | Zero-instrumentation LLM proxy |
| `GET` | `/v1/llm/overview` | Bearer/Session | Summary: traces, tokens, cost, errors |
| `GET` | `/v1/llm/usage` | Bearer/Session | Hourly token/cost by model |
| `GET` | `/v1/llm/latency` | Bearer/Session | p50/p90/p99 latency by model |
| `GET` | `/v1/llm/models` | Bearer/Session | Per-model breakdown |
| `GET` | `/v1/llm/traces` | Bearer/Session | Paginated trace list |
| `GET` | `/v1/llm/traces/:id` | Bearer/Session | Full trace + span hierarchy |
| `GET` | `/v1/llm/export/langsmith` | Bearer/Session | Export traces as LangSmith format |
| `POST` | `/v1/llm/export/otlp` | HMAC | Import OTLP traces |
| `GET` | `/v1/llm/experiments` | Bearer/Session | List prompt A/B experiments |
| `POST` | `/v1/llm/experiments` | Bearer/Session | Create new experiment |
| `GET` | `/v1/llm/experiments/:id` | Bearer/Session | Get experiment details |
| `GET` | `/v1/llm/experiments/:id/compare` | Bearer/Session | Compare experiment variants |
| `GET` | `/v1/llm/settings` | Bearer/Session | Content storage policy |
| `PUT` | `/v1/llm/settings` | Bearer/Session | Update content storage policy |

#### LLM Query Parameters

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `project_id` | string | from token | Filter by project |
| `hours` | i64 | 24 | Time window (max 720) |
| `limit` | i64 | 50 | Page size (max 200) |
| `model` | string | - | Filter by model name |
| `provider` | string | - | Filter by provider |
| `session_id` | string | - | Filter by session |
| `offset` | i64 | 0 | Pagination offset |

#### LLM Trace Contract

```json
{
  "id": "trace-001",
  "session_id": "session-abc",
  "user_id": "user-123",
  "name": "chat-completion",
  "status": "completed",
  "input": "User prompt text",
  "output": "AI response text",
  "metadata": {"key": "value"},
  "started_at": 1700000000000,
  "ended_at": 1700000001200,
  "spans": [
    {
      "id": "span-001",
      "parent_span_id": null,
      "span_type": "generation",
      "name": "gpt-4o call",
      "model": "gpt-4o",
      "provider": "openai",
      "input_tokens": 100,
      "output_tokens": 50,
      "cost": 0.0025,
      "latency_ms": 1200,
      "time_to_first_token_ms": 300,
      "status": "ok",
      "error_message": null,
      "input": "prompt text",
      "output": "completion text",
      "metadata": {}
    }
  ]
}
```

Span types: `generation`, `tool`, `retrieval`, `custom`

**Validation limits**: trace ID max 128 chars, max 100 spans per trace, max 50 traces per batch.

**Cost**: specify in dollars (float), stored as microdollars (integer). `$0.0025` -> `2500 micros`.

**Content storage**: per-project policy controls what gets persisted. `none` (default) strips prompts/completions at ingest.

### Query Parameters for `GET /v1/errors`

| Param | Type | Description |
|-------|------|-------------|
| `release` | string | Filter by release |
| `environment` | string | Filter by environment |
| `source` | string | Filter by source (ios/android/api) |
| `route` | string | Filter by route |
| `status` | string | Filter by status (unresolved/resolved/ignored) |
| `since` | i64 | Epoch ms lower bound on last_seen |
| `until` | i64 | Epoch ms upper bound on last_seen |
| `sort` | string | Sort by: `last_seen` (default), `total_count`, `first_seen` |
| `limit` | i64 | Page size (default 50, max 200) |
| `offset` | i64 | Pagination offset |

## Event Contract

```json
{
  "timestamp": 1700000000000,
  "source": "ios",
  "environment": "prod",
  "release": "1.2.3",
  "app_version": "1.2.3",
  "build_number": "42",
  "route_or_procedure": "/api/users",
  "screen": "DashboardView",
  "error_type": "TypeError",
  "message": "Cannot read property 'id' of undefined",
  "stack": "at MyApp.handleError (src/handler.ts:42:10)\n...",
  "http_status": 500,
  "request_id": "req-abc123",
  "user_id_hash": "sha256-of-user-id",
  "device_id_hash": "sha256-of-device-id",
  "fingerprint": null,
  "metadata": {"key": "value"}
}
```

**Hard caps** (rejected if exceeded):
- Total payload: 32KB
- `stack`: 8KB
- `metadata`: 4KB
- `message`: 2KB

## Configuration

See `config.toml` for all options. Every value can be overridden via environment variables with the `BLOOP__` prefix and `__` as separator:

```bash
BLOOP__SERVER__PORT=8080
BLOOP__DATABASE__PATH=/var/lib/bloop/data.db
BLOOP__AUTH__HMAC_SECRET=your-secret-here
BLOOP__RETENTION__RAW_EVENTS_DAYS=14
```

### Alerting

Set webhook URLs via environment:

```bash
BLOOP_SLACK_WEBHOOK_URL=https://hooks.slack.com/services/...
BLOOP_WEBHOOK_URL=https://your-endpoint.com/alerts
```

### Email Alerts (SMTP)

Configure SMTP in `config.toml` to enable email alert channels:

```toml
[smtp]
enabled = true
host = "smtp.example.com"
port = 587
username = "alerts@example.com"
password = "your-password"
from = "bloop@example.com"
starttls = true
```

Then add an email channel to any alert rule via the dashboard or API.

### LLM Tracing

Requires building with `--features llm-tracing`. Configure in `config.toml`:

```toml
[llm_tracing]
enabled = true                    # runtime toggle
channel_capacity = 4096           # bounded channel size
flush_interval_secs = 2           # time-based flush trigger
flush_batch_size = 200            # count-based flush trigger
max_spans_per_trace = 100         # validation limit
max_batch_size = 50               # max traces per batch POST
default_content_storage = "none"  # none | metadata_only | full
cache_ttl_secs = 30               # query result cache TTL
```

Content storage policies:
- `none` (default) - prompts and completions stripped at ingest, never hit disk
- `metadata_only` - only metadata JSON kept
- `full` - everything stored

Per-project overrides via `PUT /v1/llm/settings?project_id=...`.

#### LLM Proxy (Zero-Instrumentation)

Use bloop as a reverse proxy to capture LLM calls without code changes:

```bash
# Configure your LLM client to use bloop as a proxy
export OPENAI_BASE_URL=http://localhost:5332/v1/proxy/openai

# Or configure in code:
const openai = new OpenAI({
  baseURL: 'http://localhost:5332/v1/proxy/openai',
  apiKey: process.env.OPENAI_API_KEY
});
```

The proxy automatically:
- Captures prompts and completions (configurable)
- Records token usage and costs
- Tracks latency and time-to-first-token
- Supports both streaming and non-streaming requests

#### LangChain / LangSmith Export

Export traces in LangSmith-compatible format:

```bash
# Export traces for LangSmith
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:5332/v1/llm/export/langsmith?project_id=default\&hours=24

# Import OTLP traces
curl -X POST http://localhost:5332/v1/llm/export/otlp \
  -H "Content-Type: application/json" \
  -H "X-Signature: $SIG" \
  -d @trace.json
```

#### Prompt A/B Testing

Create experiments to test prompt variants:

```bash
# Create experiment
curl -X POST http://localhost:5332/v1/llm/experiments \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "support-prompt-v2",
    "prompt_a": {"template": "You are a helpful assistant..."},
    "prompt_b": {"template": "You are an expert support agent..."},
    "metrics": ["cost", "latency", "error_rate"]
  }'

# Compare variants
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:5332/v1/llm/experiments/EXP_ID/compare
```

## SDKs

| Platform | Package | Repo | Install |
|----------|---------|------|---------|
| TypeScript / Node.js | `@dthink/bloop-sdk` | [bloop-js](https://github.com/jaikoo/bloop-js) | `npm install @dthink/bloop-sdk` |
| React Native | `@dthink/bloop-react-native` | [bloop-react-native](https://github.com/jaikoo/bloop-react-native) | `npm install @dthink/bloop-react-native` |
| Swift (iOS) | `BloopClient` | [bloop-swift](https://github.com/jaikoo/bloop-swift) | Swift Package Manager |
| Kotlin (Android) | `BloopClient` | [bloop-kotlin](https://github.com/jaikoo/bloop-kotlin) | Gradle |
| Python | `bloop-sdk` | [bloop-python](https://github.com/jaikoo/bloop-python) | `pip install bloop-sdk` |
| Ruby | `bloop-sdk` | [bloop-ruby](https://github.com/jaikoo/bloop-ruby) | `gem install bloop-sdk` |

All SDKs provide automatic error capture, HMAC signing, buffered batch sending, and graceful shutdown.

## Testing

```bash
# Core tests
cargo test

# LLM tracing tests
cargo test --features llm-tracing --test llm_tracing_test

# LLM proxy tests
cargo test --features llm-tracing --test llm_proxy_test
```

## Tech Stack

| Concern | Crate |
|---------|-------|
| Web framework | axum 0.8 |
| Runtime | tokio |
| SQLite | rusqlite (bundled) + deadpool-sqlite |
| Analytics / LLM queries | DuckDB (optional, feature-gated) |
| In-memory cache | moka (TinyLFU) |
| Hashing | xxhash-rust (xxh3, ~31GB/s) |
| Auth (ingest) | HMAC-SHA256 |
| Auth (dashboard) | WebAuthn / passkeys |
| Email alerts | lettre (async SMTP) |
| Alerting | reqwest (Slack/webhook) |
| Config | config (TOML + env overlay) |

## License

Apache 2.0 — see [LICENSE](LICENSE) for details.
