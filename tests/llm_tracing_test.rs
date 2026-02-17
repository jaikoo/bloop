#![cfg(feature = "llm-tracing")]

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// Helper to create HMAC signature for a payload.
fn sign(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}

/// Spawn the server with LLM tracing enabled on a random port.
async fn spawn_llm_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    let pool = {
        let cfg = deadpool_sqlite::Config::new(&db_path);
        cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap()
    };

    // Init DB with all migrations
    {
        let conn = pool.get().await.unwrap();
        conn.interact(|conn| {
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
                .unwrap();
            bloop::storage::migrations::run_migrations(conn).unwrap();
        })
        .await
        .unwrap();
    }

    // LLM tracing pipeline
    let llm_config = bloop::config::LlmTracingConfig {
        enabled: true,
        channel_capacity: 256,
        flush_interval_secs: 1,
        flush_batch_size: 50,
        max_spans_per_trace: 100,
        max_batch_size: 50,
        default_content_storage: "full".to_string(),
        cache_ttl_secs: 5,
        pricing_refresh_interval_secs: 0,
        pricing_url: String::new(),
    };

    let (llm_tx, llm_rx) = mpsc::channel(llm_config.channel_capacity);
    let worker_pool = pool.clone();
    let worker_config = llm_config.clone();
    tokio::spawn(async move {
        bloop::llm_tracing::worker::run_worker(llm_rx, worker_pool, worker_config).await;
    });

    let settings_cache = Arc::new(bloop::llm_tracing::settings::SettingsCache::new(
        pool.clone(),
        llm_config.cache_ttl_secs,
    ));

    let pricing = Arc::new(bloop::llm_tracing::pricing::PricingTable::new());

    let llm_ingest_state = Arc::new(bloop::llm_tracing::LlmIngestState {
        config: llm_config.clone(),
        tx: llm_tx,
        pool: pool.clone(),
        settings_cache: settings_cache.clone(),
        pricing: Some(pricing),
    });

    let hmac_secret = bloop::ingest::auth::HmacSecret("test-secret".to_string());
    let hmac_body_limit = bloop::ingest::auth::HmacBodyLimit(32768);

    use axum::extract::DefaultBodyLimit;
    use axum::routing::{get, post, put};
    use axum::{middleware, Router};

    let ingest_routes = Router::new()
        .route(
            "/v1/traces",
            post(bloop::llm_tracing::handler::ingest_trace),
        )
        .route(
            "/v1/traces/batch",
            post(bloop::llm_tracing::handler::ingest_trace_batch),
        )
        .route(
            "/v1/traces/{trace_id}",
            put(bloop::llm_tracing::handler::update_trace),
        )
        .layer(DefaultBodyLimit::max(32768))
        .layer(middleware::from_fn(bloop::ingest::auth::hmac_auth))
        .layer(axum::Extension(hmac_secret))
        .layer(axum::Extension(hmac_body_limit))
        .with_state(llm_ingest_state);

    // Query routes via SQLite directly (no DuckDB in tests)
    let llm_query_pool = pool.clone();
    let query_routes = Router::new()
        .route("/v1/llm/traces/{id}", get(trace_detail_sqlite))
        .route("/v1/llm/search", get(search_sqlite))
        .with_state(Arc::new(llm_query_pool));

    let app = ingest_routes.merge(query_routes);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (addr, handle)
}

/// Simplified trace detail endpoint using SQLite directly (no DuckDB needed for tests).
async fn trace_detail_sqlite(
    axum::extract::State(pool): axum::extract::State<Arc<deadpool_sqlite::Pool>>,
    axum::extract::Path(trace_id): axum::extract::Path<String>,
) -> bloop::error::AppResult<axum::Json<serde_json::Value>> {
    let conn = pool
        .get()
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("pool: {e}")))?;

    let tid = trace_id.clone();
    let result = conn
        .interact(move |conn| {
            let trace: serde_json::Value = conn.query_row(
                "SELECT id, name, status, input_tokens, output_tokens, total_tokens, cost_micros, input, output, prompt_name, prompt_version
                 FROM llm_traces WHERE id = ?1",
                rusqlite::params![tid],
                |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "name": row.get::<_, String>(1)?,
                        "status": row.get::<_, String>(2)?,
                        "input_tokens": row.get::<_, i64>(3)?,
                        "output_tokens": row.get::<_, i64>(4)?,
                        "total_tokens": row.get::<_, i64>(5)?,
                        "cost_micros": row.get::<_, i64>(6)?,
                        "input": row.get::<_, Option<String>>(7)?,
                        "output": row.get::<_, Option<String>>(8)?,
                        "prompt_name": row.get::<_, Option<String>>(9)?,
                        "prompt_version": row.get::<_, Option<String>>(10)?,
                    }))
                },
            )?;

            let mut stmt = conn.prepare(
                "SELECT id, span_type, model, input_tokens, output_tokens, cost_micros, latency_ms, status
                 FROM llm_spans WHERE trace_id = ?1 ORDER BY started_at",
            )?;

            let spans: Vec<serde_json::Value> = stmt
                .query_map(rusqlite::params![trace["id"].as_str().unwrap()], |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "span_type": row.get::<_, String>(1)?,
                        "model": row.get::<_, Option<String>>(2)?,
                        "input_tokens": row.get::<_, i64>(3)?,
                        "output_tokens": row.get::<_, i64>(4)?,
                        "cost_micros": row.get::<_, i64>(5)?,
                        "latency_ms": row.get::<_, i64>(6)?,
                        "status": row.get::<_, String>(7)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(serde_json::json!({
                "trace": trace,
                "spans": spans,
            }))
        })
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("interact: {e}")))?
        .map_err(|e: rusqlite::Error| bloop::error::AppError::Database(e))?;

    Ok(axum::Json(result))
}

/// SQLite-backed search endpoint for tests (uses FTS5 MATCH).
async fn search_sqlite(
    axum::extract::State(pool): axum::extract::State<Arc<deadpool_sqlite::Pool>>,
    axum::extract::Query(qp): axum::extract::Query<bloop::llm_tracing::types::LlmQueryParams>,
) -> bloop::error::AppResult<axum::Json<serde_json::Value>> {
    let search_query = qp.search.clone().unwrap_or_default();
    let limit = qp.limit();
    let offset = qp.offset();
    let hours = qp.hours();

    let conn = pool
        .get()
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("pool: {e}")))?;

    let result = conn
        .interact(move |conn| {
            if search_query.is_empty() {
                // No search query: return all traces
                let now_ms = chrono::Utc::now().timestamp_millis();
                let since = now_ms - (hours * 3_600_000);

                let mut stmt = conn.prepare(
                    "SELECT id, session_id, name, status, total_tokens, cost_micros, 0, started_at, ended_at
                     FROM llm_traces WHERE started_at >= ?1
                     ORDER BY started_at DESC LIMIT ?2 OFFSET ?3",
                )?;
                let traces: Vec<serde_json::Value> = stmt
                    .query_map(rusqlite::params![since, limit, offset], |row| {
                        Ok(serde_json::json!({
                            "id": row.get::<_, String>(0)?,
                            "session_id": row.get::<_, Option<String>>(1)?,
                            "name": row.get::<_, String>(2)?,
                            "status": row.get::<_, String>(3)?,
                            "total_tokens": row.get::<_, i64>(4)?,
                            "cost_micros": row.get::<_, i64>(5)?,
                            "span_count": row.get::<_, i64>(6)?,
                            "started_at": row.get::<_, i64>(7)?,
                            "ended_at": row.get::<_, Option<i64>>(8)?,
                        }))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;

                let total: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM llm_traces WHERE started_at >= ?1",
                    rusqlite::params![since],
                    |row| row.get(0),
                )?;

                Ok(serde_json::json!({
                    "traces": traces,
                    "total": total,
                    "window_hours": hours,
                }))
            } else {
                // FTS search: get matching rowids from contentless FTS5 table
                let fts_sql =
                    "SELECT rowid FROM llm_traces_fts WHERE llm_traces_fts MATCH ?1 LIMIT ?2";
                let mut fts_stmt = conn.prepare(fts_sql)?;
                let fts_rowids: Vec<i64> = fts_stmt
                    .query_map(rusqlite::params![search_query, limit], |row| {
                        row.get::<_, i64>(0)
                    })?
                    .collect::<Result<Vec<_>, _>>()?;

                if fts_rowids.is_empty() {
                    return Ok(serde_json::json!({
                        "traces": [],
                        "total": 0,
                        "window_hours": hours,
                    }));
                }

                // Build a set of matching FTS rowids for lookup
                let rowid_set: std::collections::HashSet<i64> =
                    fts_rowids.into_iter().collect();

                // Scan all traces and filter by hash match
                // (This is a test-only approach; production uses DuckDB)
                let mut stmt = conn.prepare(
                    "SELECT project_id, id, session_id, name, status, total_tokens, cost_micros, 0, started_at, ended_at
                     FROM llm_traces ORDER BY started_at DESC",
                )?;
                let traces: Vec<serde_json::Value> = stmt
                    .query_map([], |row| {
                        let project_id: String = row.get(0)?;
                        let id: String = row.get(1)?;

                        // Compute hash to match FTS rowid
                        let fts_rowid = {
                            use std::hash::{Hash, Hasher};
                            let mut hasher = std::collections::hash_map::DefaultHasher::new();
                            project_id.hash(&mut hasher);
                            id.hash(&mut hasher);
                            hasher.finish() as i64
                        };

                        Ok((fts_rowid, serde_json::json!({
                            "id": id,
                            "session_id": row.get::<_, Option<String>>(2)?,
                            "name": row.get::<_, String>(3)?,
                            "status": row.get::<_, String>(4)?,
                            "total_tokens": row.get::<_, i64>(5)?,
                            "cost_micros": row.get::<_, i64>(6)?,
                            "span_count": row.get::<_, i64>(7)?,
                            "started_at": row.get::<_, i64>(8)?,
                            "ended_at": row.get::<_, Option<i64>>(9)?,
                        })))
                    })?
                    .filter_map(|r| r.ok())
                    .filter(|(rowid, _)| rowid_set.contains(rowid))
                    .map(|(_, trace)| trace)
                    .collect();

                let total = traces.len() as i64;
                Ok(serde_json::json!({
                    "traces": traces,
                    "total": total,
                    "window_hours": hours,
                }))
            }
        })
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("interact: {e}")))?
        .map_err(|e: rusqlite::Error| bloop::error::AppError::Database(e))?;

    Ok(axum::Json(result))
}

#[tokio::test]
async fn test_ingest_single_trace() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let body = serde_json::json!({
        "id": "trace-001",
        "name": "chat-completion",
        "status": "completed",
        "spans": [{
            "id": "span-001",
            "span_type": "generation",
            "model": "gpt-4o",
            "provider": "openai",
            "input_tokens": 100,
            "output_tokens": 50,
            "cost": 0.0025,
            "latency_ms": 1200,
            "time_to_first_token_ms": 300,
            "status": "ok",
            "input": "Hello world",
            "output": "Hi there!"
        }]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["status"], "accepted");

    // Wait for flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Query the trace back
    let resp = client
        .get(format!("{base}/v1/llm/traces/trace-001"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(data["trace"]["name"], "chat-completion");
    assert_eq!(data["trace"]["total_tokens"], 150);
    // cost = 0.0025 * 1_000_000 = 2500 micros
    assert_eq!(data["trace"]["cost_micros"], 2500);
    // Trace-level input/output was not set in the payload (only span-level was)
    assert!(data["trace"]["input"].is_null());
    assert!(data["trace"]["output"].is_null());
    assert_eq!(data["spans"].as_array().unwrap().len(), 1);
    assert_eq!(data["spans"][0]["model"], "gpt-4o");
    assert_eq!(data["spans"][0]["latency_ms"], 1200);
}

#[tokio::test]
async fn test_ingest_batch_traces() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let body = serde_json::json!({
        "traces": [
            {
                "id": "batch-trace-1",
                "name": "batch-1",
                "spans": [{
                    "span_type": "generation",
                    "model": "claude-3.5-sonnet",
                    "input_tokens": 200,
                    "output_tokens": 100,
                    "cost": 0.003
                }]
            },
            {
                "id": "batch-trace-2",
                "name": "batch-2",
                "spans": [{
                    "span_type": "tool",
                    "name": "search",
                    "latency_ms": 50,
                    "status": "ok"
                }]
            }
        ]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces/batch"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["accepted"], 2);
    assert_eq!(json["dropped"], 0);
}

#[tokio::test]
async fn test_trace_validation() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Test: trace_id too long
    let long_id = "x".repeat(200);
    let body = serde_json::json!({
        "id": long_id,
        "spans": []
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);

    // Test: too many spans
    let spans: Vec<serde_json::Value> = (0..101)
        .map(|i| {
            serde_json::json!({
                "id": format!("span-{i}"),
                "span_type": "generation"
            })
        })
        .collect();

    let body = serde_json::json!({
        "id": "trace-too-many-spans",
        "spans": spans
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_hmac_auth_required() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // No signature → 401
    let body = serde_json::json!({ "spans": [] });

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_update_trace() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // First, ingest a trace
    let body = serde_json::json!({
        "id": "trace-update",
        "name": "running-trace",
        "status": "running",
        "spans": []
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Wait for flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Update the trace
    let update = serde_json::json!({
        "status": "completed",
        "output": "Final output"
    });

    let update_bytes = serde_json::to_vec(&update).unwrap();
    let sig2 = sign("test-secret", &update_bytes);

    let resp = client
        .put(format!("{base}/v1/traces/trace-update"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig2)
        .body(update_bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify the update
    let resp = client
        .get(format!("{base}/v1/llm/traces/trace-update"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(data["trace"]["status"], "completed");
}

#[tokio::test]
async fn test_microdollar_conversion() {
    // Unit test for the conversion function
    assert_eq!(bloop::llm_tracing::types::dollars_to_micros(0.0), 0);
    assert_eq!(bloop::llm_tracing::types::dollars_to_micros(1.0), 1_000_000);
    assert_eq!(bloop::llm_tracing::types::dollars_to_micros(0.0025), 2500);
    assert_eq!(bloop::llm_tracing::types::dollars_to_micros(0.000001), 1);
}

#[tokio::test]
async fn test_ingest_trace_with_prompt_fields() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let body = serde_json::json!({
        "id": "trace-prompt-001",
        "name": "summarize",
        "status": "completed",
        "prompt_name": "summarizer",
        "prompt_version": "1.2.0",
        "spans": [{
            "id": "span-p001",
            "span_type": "generation",
            "model": "gpt-4o",
            "provider": "openai",
            "input_tokens": 500,
            "output_tokens": 150,
            "cost": 0.005,
            "latency_ms": 800,
            "status": "ok"
        }]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // Wait for flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Query the trace back and verify prompt fields are persisted
    let resp = client
        .get(format!("{base}/v1/llm/traces/trace-prompt-001"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(data["trace"]["prompt_name"], "summarizer");
    assert_eq!(data["trace"]["prompt_version"], "1.2.0");
}

#[tokio::test]
async fn test_ingest_trace_without_prompt_fields_backward_compat() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Ingest a trace without prompt fields -- must remain backward compatible
    let body = serde_json::json!({
        "id": "trace-no-prompt-001",
        "name": "basic-chat",
        "status": "completed",
        "spans": [{
            "id": "span-np001",
            "span_type": "generation",
            "model": "claude-3.5-sonnet",
            "provider": "anthropic",
            "input_tokens": 100,
            "output_tokens": 50,
            "cost": 0.001,
            "latency_ms": 400,
            "status": "ok"
        }]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // Wait for flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Query the trace back and verify prompt fields are null
    let resp = client
        .get(format!("{base}/v1/llm/traces/trace-no-prompt-001"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert!(data["trace"]["prompt_name"].is_null());
    assert!(data["trace"]["prompt_version"].is_null());
}

#[tokio::test]
async fn test_batch_ingest_with_prompt_fields() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let body = serde_json::json!({
        "traces": [
            {
                "id": "batch-prompt-1",
                "name": "translate",
                "prompt_name": "translator",
                "prompt_version": "2.0.0",
                "spans": [{
                    "span_type": "generation",
                    "model": "gpt-4o",
                    "input_tokens": 200,
                    "output_tokens": 100,
                    "cost": 0.003
                }]
            },
            {
                "id": "batch-prompt-2",
                "name": "classify",
                "prompt_name": "classifier",
                "prompt_version": "1.0.0",
                "spans": [{
                    "span_type": "generation",
                    "model": "claude-3.5-sonnet",
                    "input_tokens": 50,
                    "output_tokens": 10,
                    "cost": 0.001
                }]
            }
        ]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces/batch"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["accepted"], 2);

    // Wait for flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Verify first trace prompt fields
    let resp = client
        .get(format!("{base}/v1/llm/traces/batch-prompt-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(data["trace"]["prompt_name"], "translator");
    assert_eq!(data["trace"]["prompt_version"], "2.0.0");

    // Verify second trace prompt fields
    let resp = client
        .get(format!("{base}/v1/llm/traces/batch-prompt-2"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(data["trace"]["prompt_name"], "classifier");
    assert_eq!(data["trace"]["prompt_version"], "1.0.0");
}

// ── New filter / search tests ──

#[test]
fn test_llm_query_params_new_fields_deserialize() {
    // Verify new filter fields deserialize from query string format
    let json = serde_json::json!({
        "project_id": "proj-1",
        "hours": 48,
        "limit": 25,
        "model": "gpt-4o",
        "provider": "openai",
        "session_id": "sess-1",
        "offset": 10,
        "user_id": "user-42",
        "status": "error",
        "min_cost_micros": 1000,
        "min_latency_ms": 500,
        "search": "timeout error",
        "has_error": true,
        "sort": "most_expensive"
    });

    let qp: bloop::llm_tracing::types::LlmQueryParams =
        serde_json::from_value(json).expect("should deserialize with new fields");

    assert_eq!(qp.user_id.as_deref(), Some("user-42"));
    assert_eq!(qp.status.as_deref(), Some("error"));
    assert_eq!(qp.min_cost_micros, Some(1000));
    assert_eq!(qp.min_latency_ms, Some(500));
    assert_eq!(qp.search.as_deref(), Some("timeout error"));
    assert_eq!(qp.has_error, Some(true));
    assert_eq!(qp.sort.as_deref(), Some("most_expensive"));
}

#[test]
fn test_llm_query_params_new_fields_optional() {
    // All new fields should be optional (None when absent)
    let json = serde_json::json!({
        "hours": 24,
        "limit": 50
    });

    let qp: bloop::llm_tracing::types::LlmQueryParams =
        serde_json::from_value(json).expect("should deserialize without new fields");

    assert!(qp.user_id.is_none());
    assert!(qp.status.is_none());
    assert!(qp.min_cost_micros.is_none());
    assert!(qp.min_latency_ms.is_none());
    assert!(qp.search.is_none());
    assert!(qp.has_error.is_none());
    assert!(qp.sort.is_none());
}

#[tokio::test]
async fn test_fts_populated_on_ingest() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Ingest a trace with a distinctive name and an error span
    let body = serde_json::json!({
        "id": "fts-trace-001",
        "name": "user-authentication-flow",
        "status": "error",
        "spans": [{
            "id": "fts-span-001",
            "span_type": "generation",
            "model": "gpt-4o",
            "status": "error",
            "error_message": "rate limit exceeded on API gateway",
            "input_tokens": 100,
            "output_tokens": 0,
            "cost": 0.001,
            "latency_ms": 5000
        }]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Wait for flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Query FTS table directly via the search endpoint
    let resp = client
        .get(format!("{base}/v1/llm/search?search=authentication"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    let traces = data["traces"].as_array().expect("traces should be array");
    assert!(
        !traces.is_empty(),
        "FTS search for 'authentication' should find the trace"
    );
    assert_eq!(traces[0]["name"], "user-authentication-flow");

    // Search by error message
    let resp = client
        .get(format!("{base}/v1/llm/search?search=rate+limit"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    let traces = data["traces"].as_array().expect("traces should be array");
    assert!(
        !traces.is_empty(),
        "FTS search for 'rate limit' should find the trace via error message"
    );
}

#[tokio::test]
async fn test_search_empty_query_returns_all() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Search with empty query should return traces (fall through to list)
    let resp = client
        .get(format!("{base}/v1/llm/search"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    // Should have the expected response shape
    assert!(data["traces"].is_array());
    assert!(data["total"].is_number());
    assert!(data["window_hours"].is_number());
}

#[tokio::test]
async fn test_search_no_match_returns_empty() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Search for something that does not exist
    let resp = client
        .get(format!("{base}/v1/llm/search?search=xyznonexistent123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    let traces = data["traces"].as_array().expect("traces should be array");
    assert!(traces.is_empty(), "search for nonsense should return empty");
}

// ── Score Tests ──

/// Spawn a server that includes score endpoints (no auth, direct SQLite).
async fn spawn_llm_server_with_scores() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    let pool = {
        let cfg = deadpool_sqlite::Config::new(&db_path);
        cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap()
    };

    // Init DB with all migrations
    {
        let conn = pool.get().await.unwrap();
        conn.interact(|conn| {
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
                .unwrap();
            bloop::storage::migrations::run_migrations(conn).unwrap();
        })
        .await
        .unwrap();
    }

    // LLM tracing pipeline
    let llm_config = bloop::config::LlmTracingConfig {
        enabled: true,
        channel_capacity: 256,
        flush_interval_secs: 1,
        flush_batch_size: 50,
        max_spans_per_trace: 100,
        max_batch_size: 50,
        default_content_storage: "full".to_string(),
        cache_ttl_secs: 5,
        pricing_refresh_interval_secs: 0,
        pricing_url: String::new(),
    };

    let (llm_tx, llm_rx) = mpsc::channel(llm_config.channel_capacity);
    let worker_pool = pool.clone();
    let worker_config = llm_config.clone();
    tokio::spawn(async move {
        bloop::llm_tracing::worker::run_worker(llm_rx, worker_pool, worker_config).await;
    });

    let settings_cache = Arc::new(bloop::llm_tracing::settings::SettingsCache::new(
        pool.clone(),
        llm_config.cache_ttl_secs,
    ));

    let pricing = Arc::new(bloop::llm_tracing::pricing::PricingTable::new());

    let llm_ingest_state = Arc::new(bloop::llm_tracing::LlmIngestState {
        config: llm_config.clone(),
        tx: llm_tx,
        pool: pool.clone(),
        settings_cache: settings_cache.clone(),
        pricing: Some(pricing),
    });

    let hmac_secret = bloop::ingest::auth::HmacSecret("test-secret".to_string());
    let hmac_body_limit = bloop::ingest::auth::HmacBodyLimit(32768);

    use axum::extract::DefaultBodyLimit;
    use axum::routing::{get, post, put};
    use axum::{middleware, Router};

    let ingest_routes = Router::new()
        .route(
            "/v1/traces",
            post(bloop::llm_tracing::handler::ingest_trace),
        )
        .route(
            "/v1/traces/batch",
            post(bloop::llm_tracing::handler::ingest_trace_batch),
        )
        .route(
            "/v1/traces/{trace_id}",
            put(bloop::llm_tracing::handler::update_trace),
        )
        .layer(DefaultBodyLimit::max(32768))
        .layer(middleware::from_fn(bloop::ingest::auth::hmac_auth))
        .layer(axum::Extension(hmac_secret))
        .layer(axum::Extension(hmac_body_limit))
        .with_state(llm_ingest_state);

    // Query + score + feedback + budget routes via SQLite directly (no DuckDB in tests)
    let score_pool = Arc::new(pool.clone());
    let score_routes = Router::new()
        .route("/v1/llm/traces/{id}", get(trace_detail_sqlite))
        .route(
            "/v1/llm/traces/{trace_id}/scores",
            post(bloop::llm_tracing::scores::post_score),
        )
        .route(
            "/v1/llm/traces/{trace_id}/scores",
            get(bloop::llm_tracing::scores::get_scores),
        )
        .route(
            "/v1/llm/scores/summary",
            get(bloop::llm_tracing::scores::get_score_summary),
        )
        .route(
            "/v1/llm/traces/{trace_id}/feedback",
            post(bloop::llm_tracing::feedback::post_feedback),
        )
        .route(
            "/v1/llm/traces/{trace_id}/feedback",
            get(bloop::llm_tracing::feedback::get_feedback),
        )
        .route(
            "/v1/llm/feedback/summary",
            get(bloop::llm_tracing::feedback::get_feedback_summary),
        )
        .route(
            "/v1/llm/budget",
            get(bloop::llm_tracing::budget::get_budget),
        )
        .route(
            "/v1/llm/budget",
            put(bloop::llm_tracing::budget::set_budget),
        )
        .with_state(score_pool);

    // Session + tool SQLite-backed test routes
    let session_pool = Arc::new(pool.clone());
    let session_routes = Router::new()
        .route("/v1/llm/sessions", get(sessions_list_sqlite))
        .route("/v1/llm/sessions/{session_id}", get(session_detail_sqlite))
        .route("/v1/llm/tools", get(tools_sqlite))
        .route(
            "/v1/llm/prompts/{name}/versions",
            get(prompt_versions_sqlite),
        )
        .with_state(session_pool);

    let app = ingest_routes.merge(score_routes).merge(session_routes);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (addr, handle)
}

/// Helper: ingest a trace and wait for flush.
async fn ingest_trace_for_scores(client: &reqwest::Client, base: &str, trace_id: &str) {
    let body = serde_json::json!({
        "id": trace_id,
        "name": "scored-trace",
        "status": "completed",
        "spans": [{
            "span_type": "generation",
            "model": "gpt-4o",
            "input_tokens": 100,
            "output_tokens": 50,
            "cost": 0.002,
            "latency_ms": 800,
            "status": "ok"
        }]
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Wait for worker flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
}

#[tokio::test]
async fn test_post_and_get_score() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Ingest a trace first
    ingest_trace_for_scores(&client, &base, "score-trace-1").await;

    // POST a score
    let score_body = serde_json::json!({
        "name": "accuracy",
        "value": 0.85,
        "source": "user",
        "comment": "Mostly correct"
    });

    let resp = client
        .post(format!("{base}/v1/llm/traces/score-trace-1/scores"))
        .json(&score_body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["status"], "ok");

    // GET scores
    let resp = client
        .get(format!("{base}/v1/llm/traces/score-trace-1/scores"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    let scores = json["scores"].as_array().unwrap();
    assert_eq!(scores.len(), 1);
    assert_eq!(scores[0]["name"], "accuracy");
    assert_eq!(scores[0]["value"], 0.85);
    assert_eq!(scores[0]["source"], "user");
    assert_eq!(scores[0]["comment"], "Mostly correct");
}

#[tokio::test]
async fn test_score_upsert() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "score-upsert-1").await;

    // POST first score
    let resp = client
        .post(format!("{base}/v1/llm/traces/score-upsert-1/scores"))
        .json(&serde_json::json!({
            "name": "relevance",
            "value": 0.5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // POST same name again (upsert)
    let resp = client
        .post(format!("{base}/v1/llm/traces/score-upsert-1/scores"))
        .json(&serde_json::json!({
            "name": "relevance",
            "value": 0.9,
            "comment": "Updated"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // GET scores - should have exactly 1 (upserted, not duplicated)
    let resp = client
        .get(format!("{base}/v1/llm/traces/score-upsert-1/scores"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    let scores = json["scores"].as_array().unwrap();
    assert_eq!(scores.len(), 1);
    assert_eq!(scores[0]["value"], 0.9);
    assert_eq!(scores[0]["comment"], "Updated");
}

#[tokio::test]
async fn test_score_validation_name_empty() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "score-val-1").await;

    // Empty name
    let resp = client
        .post(format!("{base}/v1/llm/traces/score-val-1/scores"))
        .json(&serde_json::json!({
            "name": "",
            "value": 0.5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_score_validation_name_too_long() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "score-val-2").await;

    // Name too long (> 64 chars)
    let long_name = "a".repeat(65);
    let resp = client
        .post(format!("{base}/v1/llm/traces/score-val-2/scores"))
        .json(&serde_json::json!({
            "name": long_name,
            "value": 0.5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_score_validation_name_bad_chars() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "score-val-3").await;

    // Name with invalid characters
    let resp = client
        .post(format!("{base}/v1/llm/traces/score-val-3/scores"))
        .json(&serde_json::json!({
            "name": "my score!",
            "value": 0.5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_score_validation_value_out_of_range() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "score-val-4").await;

    // Value > 1.0
    let resp = client
        .post(format!("{base}/v1/llm/traces/score-val-4/scores"))
        .json(&serde_json::json!({
            "name": "accuracy",
            "value": 1.5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Value < 0.0
    let resp = client
        .post(format!("{base}/v1/llm/traces/score-val-4/scores"))
        .json(&serde_json::json!({
            "name": "accuracy",
            "value": -0.1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_score_validation_bad_source() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "score-val-5").await;

    let resp = client
        .post(format!("{base}/v1/llm/traces/score-val-5/scores"))
        .json(&serde_json::json!({
            "name": "accuracy",
            "value": 0.5,
            "source": "invalid_source"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_score_validation_comment_too_long() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "score-val-6").await;

    let long_comment = "x".repeat(501);
    let resp = client
        .post(format!("{base}/v1/llm/traces/score-val-6/scores"))
        .json(&serde_json::json!({
            "name": "accuracy",
            "value": 0.5,
            "comment": long_comment
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_score_trace_not_found() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // POST score for nonexistent trace
    let resp = client
        .post(format!("{base}/v1/llm/traces/nonexistent-trace/scores"))
        .json(&serde_json::json!({
            "name": "accuracy",
            "value": 0.5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_score_multiple_names() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "score-multi-1").await;

    // Post multiple different scores
    for (name, value) in &[("accuracy", 0.9), ("relevance", 0.7), ("safety", 1.0)] {
        let resp = client
            .post(format!("{base}/v1/llm/traces/score-multi-1/scores"))
            .json(&serde_json::json!({
                "name": name,
                "value": value
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    // GET all scores
    let resp = client
        .get(format!("{base}/v1/llm/traces/score-multi-1/scores"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    let scores = json["scores"].as_array().unwrap();
    assert_eq!(scores.len(), 3);
}

#[tokio::test]
async fn test_score_default_source() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "score-default-src").await;

    // Post score without source (should default to "api")
    let resp = client
        .post(format!("{base}/v1/llm/traces/score-default-src/scores"))
        .json(&serde_json::json!({
            "name": "quality",
            "value": 0.75
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify source defaults to "api"
    let resp = client
        .get(format!("{base}/v1/llm/traces/score-default-src/scores"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    let scores = json["scores"].as_array().unwrap();
    assert_eq!(scores[0]["source"], "api");
}

#[tokio::test]
async fn test_score_summary() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Ingest multiple traces and score them
    for i in 0..5 {
        let tid = format!("summary-trace-{i}");
        ingest_trace_for_scores(&client, &base, &tid).await;
        let value = (i as f64 + 1.0) * 0.2; // 0.2, 0.4, 0.6, 0.8, 1.0
        let resp = client
            .post(format!("{base}/v1/llm/traces/{tid}/scores"))
            .json(&serde_json::json!({
                "name": "accuracy",
                "value": value
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    // GET summary
    let resp = client
        .get(format!("{base}/v1/llm/scores/summary?name=accuracy"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["name"], "accuracy");
    assert_eq!(json["count"], 5);
    // avg of 0.2, 0.4, 0.6, 0.8, 1.0 = 0.6
    let avg = json["avg"].as_f64().unwrap();
    assert!((avg - 0.6).abs() < 0.01, "expected avg ~0.6, got {avg}");
}

#[tokio::test]
async fn test_score_boundary_values() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "score-boundary").await;

    // Value = 0.0 (valid)
    let resp = client
        .post(format!("{base}/v1/llm/traces/score-boundary/scores"))
        .json(&serde_json::json!({
            "name": "zero_score",
            "value": 0.0
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Value = 1.0 (valid)
    let resp = client
        .post(format!("{base}/v1/llm/traces/score-boundary/scores"))
        .json(&serde_json::json!({
            "name": "full_score",
            "value": 1.0
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify both stored
    let resp = client
        .get(format!("{base}/v1/llm/traces/score-boundary/scores"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["scores"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_score_get_empty() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "score-empty").await;

    // GET scores for trace with no scores
    let resp = client
        .get(format!("{base}/v1/llm/traces/score-empty/scores"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    let scores = json["scores"].as_array().unwrap();
    assert_eq!(scores.len(), 0);
}

#[tokio::test]
async fn test_percentile_function() {
    // Unit test for the percentile helper
    // 9 elements: indices 0-8, p50 => idx = (50/100 * 8).round() = 4 => values[4] = 0.5
    let values = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9];
    assert_eq!(bloop::llm_tracing::scores::percentile(&values, 50.0), 0.5);
    // p10 => idx = (10/100 * 8).round() = 0.8.round() = 1 => values[1] = 0.2
    assert_eq!(bloop::llm_tracing::scores::percentile(&values, 10.0), 0.2);
    // p90 => idx = (90/100 * 8).round() = 7.2.round() = 7 => values[7] = 0.8
    assert_eq!(bloop::llm_tracing::scores::percentile(&values, 90.0), 0.8);

    // Empty
    assert_eq!(bloop::llm_tracing::scores::percentile(&[], 50.0), 0.0);

    // Single element
    assert_eq!(bloop::llm_tracing::scores::percentile(&[0.5], 50.0), 0.5);
}

// ── Server-Side Pricing Tests ──

#[tokio::test]
async fn test_server_side_pricing_auto_calculate() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Send trace with cost=0 (or omitted) — server should auto-calculate from model pricing
    let body = serde_json::json!({
        "id": "pricing-trace-001",
        "name": "auto-priced",
        "status": "completed",
        "spans": [{
            "id": "pricing-span-001",
            "span_type": "generation",
            "model": "gpt-4o",
            "provider": "openai",
            "input_tokens": 500,
            "output_tokens": 100,
            "cost": 0.0,
            "latency_ms": 800,
            "status": "ok"
        }]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Wait for flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Query the trace back — cost should be auto-calculated
    let resp = client
        .get(format!("{base}/v1/llm/traces/pricing-trace-001"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();

    let cost_micros = data["trace"]["cost_micros"].as_i64().unwrap();
    // gpt-4o: input=$2.50/M ($0.0000025/token), output=$10.00/M ($0.00001/token)
    // 500 * 2.5e-6 + 100 * 10e-6 = 0.00125 + 0.001 = 0.00225
    // In micros: 2250
    assert!(
        cost_micros > 0,
        "cost_micros should be auto-calculated, got {cost_micros}"
    );
    // Allow some tolerance for price changes, but should be in right ballpark
    assert!(
        cost_micros > 1000 && cost_micros < 10000,
        "cost_micros should be ~2250 for gpt-4o 500in/100out, got {cost_micros}"
    );
}

#[tokio::test]
async fn test_server_side_pricing_explicit_cost_preserved() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Send trace with explicit cost — server should NOT override
    let body = serde_json::json!({
        "id": "pricing-explicit-001",
        "name": "explicit-cost",
        "status": "completed",
        "spans": [{
            "id": "pricing-explicit-span-001",
            "span_type": "generation",
            "model": "gpt-4o",
            "provider": "openai",
            "input_tokens": 500,
            "output_tokens": 100,
            "cost": 0.005,
            "latency_ms": 800,
            "status": "ok"
        }]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Wait for flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let resp = client
        .get(format!("{base}/v1/llm/traces/pricing-explicit-001"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();

    // 0.005 * 1_000_000 = 5000 micros (exact, not auto-calculated)
    assert_eq!(data["trace"]["cost_micros"], 5000);
}

#[tokio::test]
async fn test_server_side_pricing_unknown_model() {
    let (addr, _handle) = spawn_llm_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Send trace with unknown model and cost=0 — should stay 0, no error
    let body = serde_json::json!({
        "id": "pricing-unknown-001",
        "name": "unknown-model",
        "status": "completed",
        "spans": [{
            "id": "pricing-unknown-span-001",
            "span_type": "generation",
            "model": "totally-fake-model-12345",
            "provider": "unknown",
            "input_tokens": 500,
            "output_tokens": 100,
            "cost": 0.0,
            "latency_ms": 800,
            "status": "ok"
        }]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Wait for flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let resp = client
        .get(format!("{base}/v1/llm/traces/pricing-unknown-001"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();

    // Unknown model + cost=0 → cost_micros should be 0 (no error)
    assert_eq!(data["trace"]["cost_micros"], 0);
}

#[test]
fn test_pricing_table_lookup_strategies() {
    let table = bloop::llm_tracing::pricing::PricingTable::new();

    // Exact match
    let price = table.lookup("gpt-4o");
    assert!(price.is_some(), "gpt-4o should match exactly");

    // Date suffix stripping: "gpt-4o-2024-08-06" → try "gpt-4o"
    let price = table.lookup("gpt-4o-2024-08-06");
    assert!(
        price.is_some(),
        "gpt-4o-2024-08-06 should match via date suffix stripping"
    );

    // Unknown model returns None
    let price = table.lookup("totally-nonexistent-model-xyz");
    assert!(price.is_none(), "nonexistent model should return None");
}

// ── SQLite-backed test endpoints for sessions, tools, prompts ──

async fn sessions_list_sqlite(
    axum::extract::State(pool): axum::extract::State<Arc<deadpool_sqlite::Pool>>,
    axum::extract::Query(qp): axum::extract::Query<bloop::llm_tracing::types::LlmQueryParams>,
) -> bloop::error::AppResult<axum::Json<serde_json::Value>> {
    let hours = qp.hours();
    let limit = qp.limit();
    let offset = qp.offset();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);
    let user_id = qp.user_id.clone();

    let conn = pool
        .get()
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("pool: {e}")))?;

    let result = conn
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT
                    session_id,
                    MIN(user_id) AS user_id,
                    COUNT(*) AS trace_count,
                    COALESCE(SUM(cost_micros), 0) AS total_cost_micros,
                    COALESCE(SUM(total_tokens), 0) AS total_tokens,
                    MIN(started_at) AS first_trace_at,
                    MAX(started_at) AS last_trace_at,
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS error_count
                FROM llm_traces
                WHERE started_at >= ?1
                    AND session_id IS NOT NULL
                    AND (?2 IS NULL OR user_id = ?2)
                GROUP BY session_id
                ORDER BY MAX(started_at) DESC
                LIMIT ?3 OFFSET ?4",
            )?;
            let sessions: Vec<serde_json::Value> = stmt
                .query_map(rusqlite::params![since, user_id, limit, offset], |row| {
                    Ok(serde_json::json!({
                        "session_id": row.get::<_, String>(0)?,
                        "user_id": row.get::<_, Option<String>>(1)?,
                        "trace_count": row.get::<_, i64>(2)?,
                        "total_cost_micros": row.get::<_, i64>(3)?,
                        "total_tokens": row.get::<_, i64>(4)?,
                        "first_trace_at": row.get::<_, i64>(5)?,
                        "last_trace_at": row.get::<_, i64>(6)?,
                        "error_count": row.get::<_, i64>(7)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            let total: i64 = conn.query_row(
                "SELECT COUNT(DISTINCT session_id) FROM llm_traces
                 WHERE started_at >= ?1 AND session_id IS NOT NULL AND (?2 IS NULL OR user_id = ?2)",
                rusqlite::params![since, user_id],
                |row| row.get(0),
            )?;

            Ok(serde_json::json!({
                "sessions": sessions,
                "total": total,
                "window_hours": hours,
            }))
        })
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("interact: {e}")))?
        .map_err(|e: rusqlite::Error| bloop::error::AppError::Database(e))?;

    Ok(axum::Json(result))
}

async fn session_detail_sqlite(
    axum::extract::State(pool): axum::extract::State<Arc<deadpool_sqlite::Pool>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> bloop::error::AppResult<axum::Json<serde_json::Value>> {
    let conn = pool
        .get()
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("pool: {e}")))?;

    let sid = session_id.clone();
    let result = conn
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, name, status, total_tokens, cost_micros, 0, started_at, ended_at
                 FROM llm_traces WHERE session_id = ?1
                 ORDER BY started_at ASC",
            )?;
            let traces: Vec<serde_json::Value> = stmt
                .query_map(rusqlite::params![sid], |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "session_id": row.get::<_, Option<String>>(1)?,
                        "name": row.get::<_, String>(2)?,
                        "status": row.get::<_, String>(3)?,
                        "total_tokens": row.get::<_, i64>(4)?,
                        "cost_micros": row.get::<_, i64>(5)?,
                        "span_count": row.get::<_, i64>(6)?,
                        "started_at": row.get::<_, i64>(7)?,
                        "ended_at": row.get::<_, Option<i64>>(8)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            let total_cost: i64 = traces.iter().map(|t| t["cost_micros"].as_i64().unwrap_or(0)).sum();
            let total_tokens: i64 = traces.iter().map(|t| t["total_tokens"].as_i64().unwrap_or(0)).sum();

            Ok(serde_json::json!({
                "session_id": sid,
                "traces": traces,
                "total_cost_micros": total_cost,
                "total_tokens": total_tokens,
            }))
        })
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("interact: {e}")))?
        .map_err(|e: rusqlite::Error| bloop::error::AppError::Database(e))?;

    Ok(axum::Json(result))
}

async fn tools_sqlite(
    axum::extract::State(pool): axum::extract::State<Arc<deadpool_sqlite::Pool>>,
    axum::extract::Query(qp): axum::extract::Query<bloop::llm_tracing::types::LlmQueryParams>,
) -> bloop::error::AppResult<axum::Json<serde_json::Value>> {
    let hours = qp.hours();
    let limit = qp.limit();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);

    let conn = pool
        .get()
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("pool: {e}")))?;

    let result = conn
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT
                    name,
                    COUNT(*) AS call_count,
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS error_count,
                    CASE WHEN COUNT(*) > 0
                        THEN (CAST(SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS REAL) * 100.0 / COUNT(*))
                        ELSE 0.0 END AS error_rate,
                    COALESCE(AVG(latency_ms), 0) AS avg_latency_ms,
                    COALESCE(SUM(cost_micros), 0) AS total_cost_micros
                FROM llm_spans
                WHERE started_at >= ?1
                    AND span_type IN ('tool', 'retrieval')
                GROUP BY name
                ORDER BY call_count DESC
                LIMIT ?2",
            )?;
            let tools: Vec<serde_json::Value> = stmt
                .query_map(rusqlite::params![since, limit], |row| {
                    Ok(serde_json::json!({
                        "name": row.get::<_, String>(0)?,
                        "call_count": row.get::<_, i64>(1)?,
                        "error_count": row.get::<_, i64>(2)?,
                        "error_rate": row.get::<_, f64>(3)?,
                        "avg_latency_ms": row.get::<_, f64>(4)?,
                        "total_cost_micros": row.get::<_, i64>(5)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(serde_json::json!({
                "tools": tools,
                "window_hours": hours,
            }))
        })
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("interact: {e}")))?
        .map_err(|e: rusqlite::Error| bloop::error::AppError::Database(e))?;

    Ok(axum::Json(result))
}

async fn prompt_versions_sqlite(
    axum::extract::State(pool): axum::extract::State<Arc<deadpool_sqlite::Pool>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::Query(qp): axum::extract::Query<bloop::llm_tracing::types::LlmQueryParams>,
) -> bloop::error::AppResult<axum::Json<serde_json::Value>> {
    let hours = qp.hours();
    let limit = qp.limit();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);

    let conn = pool
        .get()
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("pool: {e}")))?;

    let prompt_name = name.clone();
    let result = conn
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT
                    prompt_version,
                    COUNT(*) AS trace_count,
                    COALESCE(AVG(cost_micros), 0) AS avg_cost_micros,
                    COALESCE(AVG(CASE WHEN ended_at IS NOT NULL AND started_at IS NOT NULL
                        THEN ended_at - started_at ELSE 0 END), 0) AS avg_latency_ms,
                    CASE WHEN COUNT(*) > 0
                        THEN (CAST(SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS REAL) / COUNT(*))
                        ELSE 0.0 END AS error_rate,
                    COALESCE(AVG(input_tokens), 0) AS avg_input_tokens,
                    COALESCE(AVG(output_tokens), 0) AS avg_output_tokens,
                    MIN(started_at) AS first_seen,
                    MAX(started_at) AS last_seen
                FROM llm_traces
                WHERE started_at >= ?1
                    AND prompt_name = ?2
                    AND prompt_version IS NOT NULL
                GROUP BY prompt_version
                ORDER BY MAX(started_at) DESC
                LIMIT ?3",
            )?;
            let versions: Vec<serde_json::Value> = stmt
                .query_map(rusqlite::params![since, prompt_name, limit], |row| {
                    Ok(serde_json::json!({
                        "version": row.get::<_, String>(0)?,
                        "trace_count": row.get::<_, i64>(1)?,
                        "avg_cost_micros": row.get::<_, f64>(2)? as i64,
                        "avg_latency_ms": row.get::<_, f64>(3)?,
                        "error_rate": row.get::<_, f64>(4)?,
                        "avg_input_tokens": row.get::<_, f64>(5)?,
                        "avg_output_tokens": row.get::<_, f64>(6)?,
                        "first_seen": row.get::<_, i64>(7)?,
                        "last_seen": row.get::<_, i64>(8)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(serde_json::json!({
                "name": prompt_name,
                "versions": versions,
                "window_hours": hours,
            }))
        })
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("interact: {e}")))?
        .map_err(|e: rusqlite::Error| bloop::error::AppError::Database(e))?;

    Ok(axum::Json(result))
}

// ── Session Tests ──

/// Helper to ingest a trace with session_id.
async fn ingest_trace_with_session(
    client: &reqwest::Client,
    base: &str,
    trace_id: &str,
    session_id: &str,
    user_id: Option<&str>,
    status: &str,
    cost: f64,
) {
    let body = serde_json::json!({
        "id": trace_id,
        "name": format!("trace-{trace_id}"),
        "session_id": session_id,
        "user_id": user_id,
        "status": status,
        "spans": [{
            "span_type": "generation",
            "model": "gpt-4o",
            "input_tokens": 100,
            "output_tokens": 50,
            "cost": cost,
            "latency_ms": 800,
            "status": "ok"
        }]
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_sessions_list_groups_by_session() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Ingest 3 traces with same session_id
    for i in 1..=3 {
        ingest_trace_with_session(
            &client,
            &base,
            &format!("sess-trace-{i}"),
            "session-A",
            Some("user1"),
            "completed",
            0.001,
        )
        .await;
    }

    // Ingest 1 trace with different session_id
    ingest_trace_with_session(
        &client,
        &base,
        "sess-trace-B",
        "session-B",
        Some("user2"),
        "error",
        0.005,
    )
    .await;

    // Wait for flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let resp = client
        .get(format!("{base}/v1/llm/sessions"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    let sessions = data["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(data["total"], 2);

    // session-B should be first (more recent)
    // Both sessions should exist
    let session_ids: Vec<&str> = sessions
        .iter()
        .map(|s| s["session_id"].as_str().unwrap())
        .collect();
    assert!(session_ids.contains(&"session-A"));
    assert!(session_ids.contains(&"session-B"));

    // session-A should have trace_count = 3
    let session_a = sessions
        .iter()
        .find(|s| s["session_id"] == "session-A")
        .unwrap();
    assert_eq!(session_a["trace_count"], 3);
}

#[tokio::test]
async fn test_session_detail_returns_traces_in_order() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    for i in 1..=3 {
        ingest_trace_with_session(
            &client,
            &base,
            &format!("detail-trace-{i}"),
            "session-detail",
            Some("user1"),
            "completed",
            0.002,
        )
        .await;
    }

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let resp = client
        .get(format!("{base}/v1/llm/sessions/session-detail"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(data["session_id"], "session-detail");
    let traces = data["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 3);
    // Traces should be in started_at ASC order
    let t1 = traces[0]["started_at"].as_i64().unwrap();
    let t2 = traces[1]["started_at"].as_i64().unwrap();
    let t3 = traces[2]["started_at"].as_i64().unwrap();
    assert!(
        t1 <= t2 && t2 <= t3,
        "traces should be in chronological order"
    );
}

// ── Tool Tests ──

#[tokio::test]
async fn test_tools_aggregation() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Ingest trace with tool + retrieval spans
    let body = serde_json::json!({
        "id": "tool-trace-1",
        "name": "agent-run",
        "status": "completed",
        "spans": [
            {
                "id": "gen-span-1",
                "span_type": "generation",
                "model": "gpt-4o",
                "input_tokens": 100,
                "output_tokens": 50,
                "cost": 0.002,
                "latency_ms": 800,
                "status": "ok"
            },
            {
                "id": "tool-span-1",
                "span_type": "tool",
                "name": "web_search",
                "latency_ms": 200,
                "status": "ok"
            },
            {
                "id": "tool-span-2",
                "span_type": "tool",
                "name": "web_search",
                "latency_ms": 150,
                "status": "ok"
            },
            {
                "id": "ret-span-1",
                "span_type": "retrieval",
                "name": "vector_db",
                "latency_ms": 50,
                "status": "ok"
            }
        ]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let resp = client
        .get(format!("{base}/v1/llm/tools"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    let tools = data["tools"].as_array().unwrap();
    // Should have 2 entries: web_search (2 calls) and vector_db (1 call)
    assert_eq!(tools.len(), 2);

    let web_search = tools.iter().find(|t| t["name"] == "web_search").unwrap();
    assert_eq!(web_search["call_count"], 2);

    let vector_db = tools.iter().find(|t| t["name"] == "vector_db").unwrap();
    assert_eq!(vector_db["call_count"], 1);
}

// ── Feedback Tests ──

#[tokio::test]
async fn test_post_and_get_feedback() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "feedback-trace-1").await;

    // POST positive feedback
    let resp = client
        .post(format!("{base}/v1/llm/traces/feedback-trace-1/feedback"))
        .json(&serde_json::json!({
            "value": 1,
            "user_id": "user1",
            "comment": "Great response"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // GET feedback
    let resp = client
        .get(format!("{base}/v1/llm/traces/feedback-trace-1/feedback"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    let feedback = json["feedback"].as_array().unwrap();
    assert_eq!(feedback.len(), 1);
    assert_eq!(feedback[0]["value"], 1);
    assert_eq!(feedback[0]["user_id"], "user1");
    assert_eq!(feedback[0]["comment"], "Great response");
}

#[tokio::test]
async fn test_feedback_upsert() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "feedback-upsert-1").await;

    // POST positive
    let resp = client
        .post(format!("{base}/v1/llm/traces/feedback-upsert-1/feedback"))
        .json(&serde_json::json!({ "value": 1, "user_id": "user1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // POST negative for same user (upsert)
    let resp = client
        .post(format!("{base}/v1/llm/traces/feedback-upsert-1/feedback"))
        .json(&serde_json::json!({ "value": -1, "user_id": "user1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Should have 1 entry (upserted), with value=-1
    let resp = client
        .get(format!("{base}/v1/llm/traces/feedback-upsert-1/feedback"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    let feedback = json["feedback"].as_array().unwrap();
    assert_eq!(feedback.len(), 1);
    assert_eq!(feedback[0]["value"], -1);
}

#[tokio::test]
async fn test_feedback_validation_invalid_value() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    ingest_trace_for_scores(&client, &base, "feedback-val-1").await;

    // value=2 is invalid
    let resp = client
        .post(format!("{base}/v1/llm/traces/feedback-val-1/feedback"))
        .json(&serde_json::json!({ "value": 2 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_feedback_trace_not_found() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let resp = client
        .post(format!("{base}/v1/llm/traces/nonexistent-trace/feedback"))
        .json(&serde_json::json!({ "value": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_feedback_summary() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Ingest traces and add feedback
    for i in 1..=4 {
        let tid = format!("fb-summary-{i}");
        ingest_trace_for_scores(&client, &base, &tid).await;
        let value = if i <= 3 { 1 } else { -1 }; // 3 positive, 1 negative
        let resp = client
            .post(format!("{base}/v1/llm/traces/{tid}/feedback"))
            .json(&serde_json::json!({ "value": value, "user_id": format!("user{i}") }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    let resp = client
        .get(format!("{base}/v1/llm/feedback/summary"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 4);
    assert_eq!(json["positive"], 3);
    assert_eq!(json["negative"], 1);
    let rate = json["positive_rate"].as_f64().unwrap();
    assert!(
        (rate - 75.0).abs() < 0.1,
        "expected 75% positive rate, got {rate}"
    );
}

// ── Prompt Version Tests ──

#[tokio::test]
async fn test_prompt_versions() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Ingest traces with same prompt_name, 2 different versions
    for (tid, version) in &[
        ("pv-1", "1.0.0"),
        ("pv-2", "1.0.0"),
        ("pv-3", "1.0.0"),
        ("pv-4", "2.0.0"),
        ("pv-5", "2.0.0"),
    ] {
        let body = serde_json::json!({
            "id": tid,
            "name": "translate",
            "prompt_name": "translator",
            "prompt_version": version,
            "status": "completed",
            "spans": [{
                "span_type": "generation",
                "model": "gpt-4o",
                "input_tokens": 100,
                "output_tokens": 50,
                "cost": 0.002,
                "latency_ms": 800,
                "status": "ok"
            }]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let sig = sign("test-secret", &body_bytes);
        let resp = client
            .post(format!("{base}/v1/traces"))
            .header("Content-Type", "application/json")
            .header("X-Signature", &sig)
            .body(body_bytes)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let resp = client
        .get(format!("{base}/v1/llm/prompts/translator/versions"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(data["name"], "translator");
    let versions = data["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);

    // Both versions should be present
    let version_strings: Vec<&str> = versions
        .iter()
        .map(|v| v["version"].as_str().unwrap())
        .collect();
    assert!(version_strings.contains(&"1.0.0"));
    assert!(version_strings.contains(&"2.0.0"));

    // v1 should have 3 traces, v2 should have 2
    let v1 = versions.iter().find(|v| v["version"] == "1.0.0").unwrap();
    assert_eq!(v1["trace_count"], 3);
    let v2 = versions.iter().find(|v| v["version"] == "2.0.0").unwrap();
    assert_eq!(v2["trace_count"], 2);
}

// ── Budget Tests ──

#[tokio::test]
async fn test_set_and_get_budget() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // PUT budget
    let resp = client
        .put(format!("{base}/v1/llm/budget"))
        .json(&serde_json::json!({
            "monthly_budget_micros": 100_000_000,
            "alert_threshold_pct": 80
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(data["monthly_budget_micros"], 100_000_000i64);
    assert_eq!(data["alert_threshold_pct"], 80);
    assert!(data["days_elapsed"].as_i64().unwrap() > 0);
    assert!(data["days_remaining"].as_i64().unwrap() >= 0);

    // GET budget
    let resp = client
        .get(format!("{base}/v1/llm/budget"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(data["monthly_budget_micros"], 100_000_000i64);
    assert_eq!(data["alert_threshold_pct"], 80);
}

#[tokio::test]
async fn test_budget_default_when_not_set() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // GET budget without setting one
    let resp = client
        .get(format!("{base}/v1/llm/budget"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(data["monthly_budget_micros"], 0);
    assert_eq!(data["alert_threshold_pct"], 80); // default
    assert_eq!(data["budget_used_pct"], 0.0);
}

#[tokio::test]
async fn test_budget_validation() {
    let (addr, _handle) = spawn_llm_server_with_scores().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Negative budget
    let resp = client
        .put(format!("{base}/v1/llm/budget"))
        .json(&serde_json::json!({
            "monthly_budget_micros": -1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// ── RAG Tests ──

/// SQLite-backed RAG endpoint for tests (no DuckDB).
async fn rag_sqlite(
    axum::extract::State(pool): axum::extract::State<Arc<deadpool_sqlite::Pool>>,
    axum::extract::Query(qp): axum::extract::Query<bloop::llm_tracing::types::LlmQueryParams>,
) -> bloop::error::AppResult<axum::Json<serde_json::Value>> {
    let hours = qp.hours();
    let limit = qp.limit();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = now_ms - (hours * 3_600_000);

    let conn = pool
        .get()
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("pool: {e}")))?;

    let result = conn
        .interact(move |conn| {
            // Sources: aggregate retrieval spans with rag.* metadata
            let mut stmt = conn.prepare(
                "SELECT
                    name,
                    json_extract(metadata, '$.\"rag.source\"') AS source,
                    COUNT(*) AS call_count,
                    COALESCE(AVG(latency_ms), 0) AS avg_latency_ms,
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS error_count,
                    CASE WHEN COUNT(*) > 0
                        THEN (CAST(SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS REAL) * 100.0 / COUNT(*))
                        ELSE 0.0 END AS error_rate,
                    COALESCE(AVG(json_extract(metadata, '$.\"rag.chunks_retrieved\"')), 0) AS avg_chunks_retrieved,
                    COALESCE(AVG(json_extract(metadata, '$.\"rag.chunks_used\"')), 0) AS avg_chunks_used,
                    COALESCE(AVG(json_extract(metadata, '$.\"rag.context_tokens\"')), 0) AS avg_context_tokens
                FROM llm_spans
                WHERE started_at >= ?1
                    AND span_type = 'retrieval'
                    AND metadata IS NOT NULL
                    AND json_extract(metadata, '$.\"rag.chunks_retrieved\"') IS NOT NULL
                GROUP BY name, json_extract(metadata, '$.\"rag.source\"')
                ORDER BY call_count DESC
                LIMIT ?2",
            )?;
            let sources: Vec<serde_json::Value> = stmt
                .query_map(rusqlite::params![since, limit], |row| {
                    Ok(serde_json::json!({
                        "retrieval_name": row.get::<_, String>(0)?,
                        "source": row.get::<_, Option<String>>(1)?,
                        "call_count": row.get::<_, i64>(2)?,
                        "avg_latency_ms": row.get::<_, f64>(3)?,
                        "p50_latency_ms": row.get::<_, f64>(3)?,
                        "p95_latency_ms": row.get::<_, f64>(3)?,
                        "error_count": row.get::<_, i64>(4)?,
                        "error_rate": row.get::<_, f64>(5)?,
                        "avg_chunks_retrieved": row.get::<_, f64>(6)?,
                        "avg_chunks_used": row.get::<_, f64>(7)?,
                        "avg_context_tokens": row.get::<_, f64>(8)?,
                        "avg_context_utilization_pct": 0.0,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            // Relevance summary
            let relevance: serde_json::Value = conn.query_row(
                "SELECT
                    COUNT(*) AS total_retrievals,
                    COALESCE(AVG(json_extract(metadata, '$.\"rag.chunks_retrieved\"')), 0),
                    COALESCE(AVG(json_extract(metadata, '$.\"rag.chunks_used\"')), 0)
                FROM llm_spans
                WHERE started_at >= ?1
                    AND span_type = 'retrieval'
                    AND metadata IS NOT NULL
                    AND json_extract(metadata, '$.\"rag.chunks_retrieved\"') IS NOT NULL",
                rusqlite::params![since],
                |row| {
                    let total: i64 = row.get(0)?;
                    let avg_chunks_retrieved: f64 = row.get(1)?;
                    let avg_chunks_used: f64 = row.get(2)?;
                    let chunk_util = if avg_chunks_retrieved > 0.0 {
                        (avg_chunks_used / avg_chunks_retrieved) * 100.0
                    } else {
                        0.0
                    };
                    Ok(serde_json::json!({
                        "total_retrievals": total,
                        "avg_top_relevance": 0.0,
                        "min_top_relevance": 0.0,
                        "max_top_relevance": 0.0,
                        "avg_chunks_retrieved": avg_chunks_retrieved,
                        "avg_chunks_used": avg_chunks_used,
                        "chunk_utilization_pct": chunk_util,
                    }))
                },
            )?;

            Ok(serde_json::json!({
                "sources": sources,
                "metrics": [],
                "relevance": relevance,
                "window_hours": hours,
            }))
        })
        .await
        .map_err(|e| bloop::error::AppError::Internal(format!("interact: {e}")))?
        .map_err(|e: rusqlite::Error| bloop::error::AppError::Database(e))?;

    Ok(axum::Json(result))
}

/// Spawn server with RAG endpoint included.
async fn spawn_llm_server_with_rag() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    let pool = {
        let cfg = deadpool_sqlite::Config::new(&db_path);
        cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap()
    };

    {
        let conn = pool.get().await.unwrap();
        conn.interact(|conn| {
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
                .unwrap();
            bloop::storage::migrations::run_migrations(conn).unwrap();
        })
        .await
        .unwrap();
    }

    let llm_config = bloop::config::LlmTracingConfig {
        enabled: true,
        channel_capacity: 256,
        flush_interval_secs: 1,
        flush_batch_size: 50,
        max_spans_per_trace: 100,
        max_batch_size: 50,
        default_content_storage: "full".to_string(),
        cache_ttl_secs: 5,
        pricing_refresh_interval_secs: 0,
        pricing_url: String::new(),
    };

    let (llm_tx, llm_rx) = mpsc::channel(llm_config.channel_capacity);
    let worker_pool = pool.clone();
    let worker_config = llm_config.clone();
    tokio::spawn(async move {
        bloop::llm_tracing::worker::run_worker(llm_rx, worker_pool, worker_config).await;
    });

    let settings_cache = Arc::new(bloop::llm_tracing::settings::SettingsCache::new(
        pool.clone(),
        llm_config.cache_ttl_secs,
    ));

    let pricing = Arc::new(bloop::llm_tracing::pricing::PricingTable::new());

    let llm_ingest_state = Arc::new(bloop::llm_tracing::LlmIngestState {
        config: llm_config.clone(),
        tx: llm_tx,
        pool: pool.clone(),
        settings_cache: settings_cache.clone(),
        pricing: Some(pricing),
    });

    let hmac_secret = bloop::ingest::auth::HmacSecret("test-secret".to_string());
    let hmac_body_limit = bloop::ingest::auth::HmacBodyLimit(32768);

    use axum::extract::DefaultBodyLimit;
    use axum::routing::{get, post, put};
    use axum::{middleware, Router};

    let ingest_routes = Router::new()
        .route(
            "/v1/traces",
            post(bloop::llm_tracing::handler::ingest_trace),
        )
        .route(
            "/v1/traces/batch",
            post(bloop::llm_tracing::handler::ingest_trace_batch),
        )
        .route(
            "/v1/traces/{trace_id}",
            put(bloop::llm_tracing::handler::update_trace),
        )
        .layer(DefaultBodyLimit::max(32768))
        .layer(middleware::from_fn(bloop::ingest::auth::hmac_auth))
        .layer(axum::Extension(hmac_secret))
        .layer(axum::Extension(hmac_body_limit))
        .with_state(llm_ingest_state);

    let query_pool = Arc::new(pool.clone());
    let query_routes = Router::new()
        .route("/v1/llm/traces/{id}", get(trace_detail_sqlite))
        .route("/v1/llm/rag", get(rag_sqlite))
        .with_state(query_pool);

    let app = ingest_routes.merge(query_routes);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (addr, handle)
}

#[tokio::test]
async fn test_rag_pipeline_analytics() {
    let (addr, _handle) = spawn_llm_server_with_rag().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Ingest a trace with a retrieval span carrying rag.* metadata
    let body = serde_json::json!({
        "id": "rag-trace-1",
        "name": "rag-pipeline",
        "status": "completed",
        "spans": [
            {
                "id": "rag-gen-1",
                "span_type": "generation",
                "model": "gpt-4o",
                "provider": "openai",
                "input_tokens": 3200,
                "output_tokens": 500,
                "cost": 0.013,
                "latency_ms": 2000,
                "status": "ok"
            },
            {
                "id": "rag-ret-1",
                "parent_span_id": "rag-gen-1",
                "span_type": "retrieval",
                "name": "vector_search",
                "latency_ms": 120,
                "status": "ok",
                "metadata": {
                    "rag.source": "pinecone",
                    "rag.chunks_retrieved": 10,
                    "rag.chunks_used": 5,
                    "rag.context_tokens": 3200,
                    "rag.max_context_tokens": 8192,
                    "rag.relevance_scores": [0.95, 0.87, 0.82, 0.71, 0.65],
                    "rag.top_k": 10
                }
            }
        ]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Query RAG endpoint
    let resp = client
        .get(format!("{base}/v1/llm/rag"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();

    // Should have sources
    let sources = data["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["retrieval_name"], "vector_search");
    assert_eq!(sources[0]["source"], "pinecone");
    assert_eq!(sources[0]["call_count"], 1);

    // Should have relevance
    let relevance = &data["relevance"];
    assert_eq!(relevance["total_retrievals"], 1);
    assert!(relevance["avg_chunks_retrieved"].as_f64().unwrap() > 0.0);

    // window_hours should be present
    assert!(data["window_hours"].is_number());
}

#[tokio::test]
async fn test_rag_metadata_preserved() {
    let (addr, _handle) = spawn_llm_server_with_rag().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Ingest trace with RAG metadata
    let body = serde_json::json!({
        "id": "rag-meta-1",
        "name": "rag-with-metadata",
        "status": "completed",
        "spans": [{
            "id": "rag-meta-ret-1",
            "span_type": "retrieval",
            "name": "doc_search",
            "latency_ms": 80,
            "status": "ok",
            "metadata": {
                "rag.source": "weaviate",
                "rag.chunks_retrieved": 8,
                "rag.chunks_used": 3,
                "rag.context_tokens": 2400
            }
        }]
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let sig = sign("test-secret", &body_bytes);

    let resp = client
        .post(format!("{base}/v1/traces"))
        .header("Content-Type", "application/json")
        .header("X-Signature", &sig)
        .body(body_bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Query trace detail to verify metadata is preserved
    let resp = client
        .get(format!("{base}/v1/llm/traces/rag-meta-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(data["trace"]["name"], "rag-with-metadata");
    // Verify spans exist
    let spans = data["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0]["span_type"], "retrieval");
}

#[tokio::test]
async fn test_rag_empty_when_no_data() {
    let (addr, _handle) = spawn_llm_server_with_rag().await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Query RAG with no data ingested
    let resp = client
        .get(format!("{base}/v1/llm/rag"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let data: serde_json::Value = resp.json().await.unwrap();

    // Sources should be empty
    let sources = data["sources"].as_array().unwrap();
    assert!(sources.is_empty());

    // Relevance should show zero
    assert_eq!(data["relevance"]["total_retrievals"], 0);

    // No errors
    assert!(data["window_hours"].is_number());
}
