use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

// Alert state for test routes
#[allow(dead_code)]
struct AlertState {
    pool: deadpool_sqlite::Pool,
    dispatcher: Arc<bloop::alert::dispatch::AlertDispatcher>,
}

// Alert CRUD wrappers (same as main.rs)
async fn list_alerts(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
) -> bloop::error::AppResult<axum::Json<Vec<bloop::alert::rules::AlertRule>>> {
    let rules = bloop::alert::rules::list_alert_rules(&state.pool).await?;
    Ok(axum::Json(rules))
}

async fn create_alert(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::Json(input): axum::Json<bloop::alert::rules::CreateAlertRule>,
) -> bloop::error::AppResult<axum::Json<bloop::alert::rules::AlertRule>> {
    let rule = bloop::alert::rules::create_alert_rule(&state.pool, input).await?;
    Ok(axum::Json(rule))
}

async fn update_alert(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<bloop::alert::rules::UpdateAlertRule>,
) -> bloop::error::AppResult<axum::Json<bloop::alert::rules::AlertRule>> {
    let rule = bloop::alert::rules::update_alert_rule(&state.pool, id, input).await?;
    Ok(axum::Json(rule))
}

async fn delete_alert(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> bloop::error::AppResult<axum::Json<serde_json::Value>> {
    bloop::alert::rules::delete_alert_rule(&state.pool, id).await?;
    Ok(axum::Json(serde_json::json!({ "deleted": id })))
}

async fn list_channels(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> bloop::error::AppResult<axum::Json<Vec<bloop::alert::rules::AlertChannel>>> {
    let channels = bloop::alert::rules::list_channels(&state.pool, id).await?;
    Ok(axum::Json(channels))
}

async fn add_channel(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<bloop::alert::rules::CreateAlertChannel>,
) -> bloop::error::AppResult<axum::Json<bloop::alert::rules::AlertChannel>> {
    let channel = bloop::alert::rules::add_channel(&state.pool, id, input).await?;
    Ok(axum::Json(channel))
}

async fn delete_channel(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::extract::Path((id, cid)): axum::extract::Path<(i64, i64)>,
) -> bloop::error::AppResult<axum::Json<serde_json::Value>> {
    bloop::alert::rules::delete_channel(&state.pool, id, cid).await?;
    Ok(axum::Json(serde_json::json!({ "deleted": cid })))
}

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

/// Spawn the server on a random port and return the address.
async fn spawn_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    // Create temp db
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_path_buf();
    // Keep tmp alive by leaking it (test only)
    std::mem::forget(tmp);

    let pool = {
        let cfg = deadpool_sqlite::Config::new(&db_path);
        cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap()
    };

    // Init DB with all migrations
    {
        let conn = pool.get().await.unwrap();
        conn.interact(|conn| {
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;",
            )
            .unwrap();
            // Run all migrations
            bloop::storage::migrations::run_migrations(conn).unwrap();
        })
        .await
        .unwrap();
    }

    let (tx, rx) = mpsc::channel::<bloop::types::ProcessedEvent>(8192);
    let (new_fp_tx, _new_fp_rx) = mpsc::unbounded_channel();

    let aggregator = bloop::pipeline::aggregator::create_aggregator();

    // Spawn worker
    let worker_pool = pool.clone();
    let worker_agg = aggregator.clone();
    let pipeline_config = bloop::config::PipelineConfig {
        flush_interval_secs: 1,
        flush_batch_size: 100,
        sample_reservoir_size: 5,
    };
    tokio::spawn(async move {
        bloop::pipeline::worker::run_worker(
            rx,
            worker_pool,
            worker_agg,
            pipeline_config,
            new_fp_tx,
        )
        .await;
    });

    let ingest_state = Arc::new(bloop::ingest::handler::IngestState {
        config: bloop::config::IngestConfig {
            max_payload_bytes: 32768,
            max_stack_bytes: 8192,
            max_metadata_bytes: 4096,
            max_message_bytes: 2048,
            max_batch_size: 50,
            channel_capacity: 8192,
        },
        tx: tx.clone(),
    });

    let query_state = Arc::new(bloop::query::handler::QueryState {
        pool: pool.clone(),
        aggregator: aggregator.clone(),
        channel_capacity: 8192,
        channel_tx: tx.clone(),
    });

    let hmac_secret = bloop::ingest::auth::HmacSecret("test-secret".to_string());

    use axum::extract::DefaultBodyLimit;
    use axum::routing::{get, post};
    use axum::{middleware, Router};

    let ingest_routes = Router::new()
        .route("/v1/ingest", post(bloop::ingest::handler::ingest_single))
        .route(
            "/v1/ingest/batch",
            post(bloop::ingest::handler::ingest_batch),
        )
        .layer(DefaultBodyLimit::max(32768))
        .layer(middleware::from_fn(bloop::ingest::auth::hmac_auth))
        .layer(axum::Extension(hmac_secret))
        .with_state(ingest_state);

    let query_routes = Router::new()
        .route("/v1/errors", get(bloop::query::handler::list_errors))
        .route(
            "/v1/errors/{fingerprint}",
            get(bloop::query::handler::get_error_detail),
        )
        .route(
            "/v1/errors/{fingerprint}/resolve",
            post(bloop::query::handler::resolve_error),
        )
        .route("/v1/stats", get(bloop::query::handler::stats))
        .route("/health", get(bloop::query::handler::health))
        .with_state(query_state);

    // Alert state for CRUD routes
    let dispatcher = Arc::new(bloop::alert::dispatch::AlertDispatcher::new(
        None, None, None,
    ));
    let alert_state = Arc::new(AlertState {
        pool: pool.clone(),
        dispatcher: dispatcher.clone(),
    });

    use axum::routing::{delete, put};

    let alert_routes = Router::new()
        .route("/v1/alerts", get(list_alerts))
        .route("/v1/alerts", post(create_alert))
        .route("/v1/alerts/{id}", put(update_alert))
        .route("/v1/alerts/{id}", delete(delete_alert))
        .route("/v1/alerts/{id}/channels", get(list_channels))
        .route("/v1/alerts/{id}/channels", post(add_channel))
        .route("/v1/alerts/{id}/channels/{cid}", delete(delete_channel))
        .with_state(alert_state);

    // Project routes
    let project_pool = Arc::new(pool.clone());
    let project_routes = Router::new()
        .route("/v1/projects", get(bloop::project::list_projects))
        .route("/v1/projects", post(bloop::project::create_project))
        .route(
            "/v1/projects/{slug}",
            delete(bloop::project::delete_project),
        )
        .route(
            "/v1/projects/{slug}/rotate-key",
            post(bloop::project::rotate_key),
        )
        .with_state(project_pool);

    // Retention routes
    let retention_state = Arc::new(bloop::storage::retention_api::RetentionState {
        pool: pool.clone(),
        default_raw_events_days: 7,
    });
    let retention_routes = Router::new()
        .route(
            "/v1/admin/retention",
            get(bloop::storage::retention_api::get_retention),
        )
        .route(
            "/v1/admin/retention",
            put(bloop::storage::retention_api::update_retention),
        )
        .route(
            "/v1/admin/retention/purge",
            post(bloop::storage::retention_api::purge_now),
        )
        .with_state(retention_state);

    let app = Router::new()
        .merge(ingest_routes)
        .merge(query_routes)
        .merge(alert_routes)
        .merge(project_routes)
        .merge(retention_routes);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server a moment to start
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (addr, handle)
}

#[tokio::test]
async fn test_health() {
    let (addr, _handle) = spawn_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["db_ok"], true);
}

#[tokio::test]
async fn test_ingest_and_query_roundtrip() {
    let (addr, _handle) = spawn_server().await;
    let client = reqwest::Client::new();
    let secret = "test-secret";

    // Ingest an event
    let event = serde_json::json!({
        "timestamp": 1700000000000_i64,
        "source": "api",
        "environment": "prod",
        "release": "1.0.0",
        "error_type": "TypeError",
        "message": "Cannot read property 'id' of undefined",
        "route_or_procedure": "/api/users",
    });

    let body = serde_json::to_vec(&event).unwrap();
    let signature = sign(secret, &body);

    let resp = client
        .post(format!("http://{addr}/v1/ingest"))
        .header("content-type", "application/json")
        .header("x-signature", &signature)
        .body(body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "accepted");

    // Wait for pipeline flush (flush_interval_secs = 1)
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Query errors
    let resp = client
        .get(format!("http://{addr}/v1/errors"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let errors: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(
        !errors.is_empty(),
        "should have at least one error aggregate"
    );

    let first = &errors[0];
    assert_eq!(first["error_type"], "TypeError");
    assert_eq!(first["release"], "1.0.0");
    assert_eq!(first["total_count"], 1);
    assert_eq!(first["status"], "unresolved");
    assert_eq!(first["project_id"], "default");

    let fingerprint = first["fingerprint"].as_str().unwrap();

    // Query error detail
    let resp = client
        .get(format!("http://{addr}/v1/errors/{fingerprint}"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let detail: serde_json::Value = resp.json().await.unwrap();
    assert!(!detail["aggregates"].as_array().unwrap().is_empty());
    assert!(!detail["samples"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_dedup_same_error() {
    let (addr, _handle) = spawn_server().await;
    let client = reqwest::Client::new();
    let secret = "test-secret";

    // Send the same error 10 times
    for _ in 0..10 {
        let event = serde_json::json!({
            "timestamp": 1700000000000_i64,
            "source": "ios",
            "environment": "prod",
            "release": "2.0.0",
            "error_type": "NetworkError",
            "message": "Connection timed out after 5000ms",
        });

        let body = serde_json::to_vec(&event).unwrap();
        let signature = sign(secret, &body);

        client
            .post(format!("http://{addr}/v1/ingest"))
            .header("content-type", "application/json")
            .header("x-signature", &signature)
            .body(body)
            .send()
            .await
            .unwrap();
    }

    // Wait for flush
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Should be a single fingerprint with count=10
    let resp = client
        .get(format!("http://{addr}/v1/errors?release=2.0.0"))
        .send()
        .await
        .unwrap();

    let errors: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(errors.len(), 1, "should dedup to single fingerprint");
    assert_eq!(errors[0]["total_count"], 10);
}

#[tokio::test]
async fn test_resolve_error() {
    let (addr, _handle) = spawn_server().await;
    let client = reqwest::Client::new();
    let secret = "test-secret";

    let event = serde_json::json!({
        "timestamp": 1700000000000_i64,
        "source": "api",
        "environment": "staging",
        "release": "3.0.0",
        "error_type": "ValueError",
        "message": "invalid input",
    });

    let body = serde_json::to_vec(&event).unwrap();
    let signature = sign(secret, &body);

    client
        .post(format!("http://{addr}/v1/ingest"))
        .header("content-type", "application/json")
        .header("x-signature", &signature)
        .body(body)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Get the fingerprint
    let resp = client
        .get(format!("http://{addr}/v1/errors?release=3.0.0"))
        .send()
        .await
        .unwrap();
    let errors: Vec<serde_json::Value> = resp.json().await.unwrap();
    let fingerprint = errors[0]["fingerprint"].as_str().unwrap();

    // Resolve it (project_id required)
    let resp = client
        .post(format!(
            "http://{addr}/v1/errors/{fingerprint}/resolve?project_id=default"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify it's resolved
    let resp = client
        .get(format!("http://{addr}/v1/errors?release=3.0.0"))
        .send()
        .await
        .unwrap();
    let errors: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(errors[0]["status"], "resolved");
}

#[tokio::test]
async fn test_batch_ingest() {
    let (addr, _handle) = spawn_server().await;
    let client = reqwest::Client::new();
    let secret = "test-secret";

    let payload = serde_json::json!({
        "events": [
            {
                "timestamp": 1700000000000_i64,
                "source": "api",
                "environment": "prod",
                "release": "4.0.0",
                "error_type": "Error1",
                "message": "first error",
            },
            {
                "timestamp": 1700000000001_i64,
                "source": "api",
                "environment": "prod",
                "release": "4.0.0",
                "error_type": "Error2",
                "message": "second error",
            },
        ]
    });

    let body = serde_json::to_vec(&payload).unwrap();
    let signature = sign(secret, &body);

    let resp = client
        .post(format!("http://{addr}/v1/ingest/batch"))
        .header("content-type", "application/json")
        .header("x-signature", &signature)
        .body(body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let result: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(result["accepted"], 2);

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let resp = client
        .get(format!("http://{addr}/v1/errors?release=4.0.0"))
        .send()
        .await
        .unwrap();
    let errors: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(errors.len(), 2);
}

#[tokio::test]
async fn test_auth_rejection() {
    let (addr, _handle) = spawn_server().await;
    let client = reqwest::Client::new();

    let event = serde_json::json!({
        "timestamp": 1700000000000_i64,
        "source": "api",
        "environment": "prod",
        "release": "1.0.0",
        "error_type": "TypeError",
        "message": "test",
    });

    // No signature
    let resp = client
        .post(format!("http://{addr}/v1/ingest"))
        .header("content-type", "application/json")
        .json(&event)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Wrong signature
    let body = serde_json::to_vec(&event).unwrap();
    let resp = client
        .post(format!("http://{addr}/v1/ingest"))
        .header("content-type", "application/json")
        .header("x-signature", "deadbeef")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_project_scoped_queries() {
    let (addr, _handle) = spawn_server().await;
    let client = reqwest::Client::new();
    let secret = "test-secret";

    // Ingest an event (goes to default project via legacy auth)
    let event = serde_json::json!({
        "timestamp": 1700000000000_i64,
        "source": "api",
        "environment": "prod",
        "release": "5.0.0",
        "error_type": "ScopeError",
        "message": "scoped test",
    });

    let body = serde_json::to_vec(&event).unwrap();
    let signature = sign(secret, &body);

    client
        .post(format!("http://{addr}/v1/ingest"))
        .header("content-type", "application/json")
        .header("x-signature", &signature)
        .body(body)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Query with project_id=default should return results
    let resp = client
        .get(format!(
            "http://{addr}/v1/errors?project_id=default&release=5.0.0"
        ))
        .send()
        .await
        .unwrap();
    let errors: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(!errors.is_empty(), "should find errors for default project");

    // Query with project_id=nonexistent should return empty
    let resp = client
        .get(format!(
            "http://{addr}/v1/errors?project_id=nonexistent&release=5.0.0"
        ))
        .send()
        .await
        .unwrap();
    let errors: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(
        errors.is_empty(),
        "should find no errors for nonexistent project"
    );
}

// ── New integration tests ──

#[tokio::test]
async fn test_alert_crud() {
    let (addr, _handle) = spawn_server().await;
    let client = reqwest::Client::new();

    // Create alert
    let resp = client
        .post(format!("http://{addr}/v1/alerts"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "name": "Test Alert",
            "config": { "type": "new_issue" },
            "description": "A test alert rule",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let alert: serde_json::Value = resp.json().await.unwrap();
    let alert_id = alert["id"].as_i64().unwrap();
    assert_eq!(alert["name"], "Test Alert");
    assert_eq!(alert["rule_type"], "new_issue");
    assert!(alert["enabled"].as_bool().unwrap());

    // List alerts
    let resp = client
        .get(format!("http://{addr}/v1/alerts"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let alerts: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(!alerts.is_empty());

    // Update alert
    let resp = client
        .put(format!("http://{addr}/v1/alerts/{alert_id}"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "name": "Updated Alert",
            "enabled": false,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let updated: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(updated["name"], "Updated Alert");
    assert!(!updated["enabled"].as_bool().unwrap());

    // Add channel (webhook)
    let resp = client
        .post(format!("http://{addr}/v1/alerts/{alert_id}/channels"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "config": { "type": "webhook", "url": "https://example.com/hook" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ch: serde_json::Value = resp.json().await.unwrap();
    let channel_id = ch["id"].as_i64().unwrap();

    // List channels
    let resp = client
        .get(format!("http://{addr}/v1/alerts/{alert_id}/channels"))
        .send()
        .await
        .unwrap();
    let channels: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(channels.len(), 1);

    // Delete channel
    let resp = client
        .delete(format!(
            "http://{addr}/v1/alerts/{alert_id}/channels/{channel_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Delete alert
    let resp = client
        .delete(format!("http://{addr}/v1/alerts/{alert_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify deleted
    let resp = client
        .get(format!("http://{addr}/v1/alerts"))
        .send()
        .await
        .unwrap();
    let alerts: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(
        alerts.iter().all(|a| a["id"].as_i64().unwrap() != alert_id),
        "deleted alert should not appear"
    );
}

#[tokio::test]
async fn test_project_crud() {
    let (addr, _handle) = spawn_server().await;
    let client = reqwest::Client::new();

    // List projects (should have "default" from migration)
    let resp = client
        .get(format!("http://{addr}/v1/projects"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let projects: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(!projects.is_empty(), "should have default project");

    // Create a new project
    let resp = client
        .post(format!("http://{addr}/v1/projects"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "name": "Test Project",
            "slug": "test-project",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let proj: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(proj["name"], "Test Project");
    assert_eq!(proj["slug"], "test-project");

    // Rotate key
    let resp = client
        .post(format!("http://{addr}/v1/projects/test-project/rotate-key"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Delete project
    let resp = client
        .delete(format!("http://{addr}/v1/projects/test-project"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_email_channel_validation() {
    let (addr, _handle) = spawn_server().await;
    let client = reqwest::Client::new();

    // Create an alert first
    let resp = client
        .post(format!("http://{addr}/v1/alerts"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "name": "Email Test Alert",
            "config": { "type": "new_issue" },
        }))
        .send()
        .await
        .unwrap();
    let alert: serde_json::Value = resp.json().await.unwrap();
    let alert_id = alert["id"].as_i64().unwrap();

    // Add email channel
    let resp = client
        .post(format!("http://{addr}/v1/alerts/{alert_id}/channels"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "config": { "type": "email", "to": "alerts@example.com" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ch: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(ch["channel_type"], "email");

    // List channels — should include email
    let resp = client
        .get(format!("http://{addr}/v1/alerts/{alert_id}/channels"))
        .send()
        .await
        .unwrap();
    let channels: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["config"]["type"], "email");
    assert_eq!(channels[0]["config"]["to"], "alerts@example.com");

    // Invalid email should be rejected
    let resp = client
        .post(format!("http://{addr}/v1/alerts/{alert_id}/channels"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "config": { "type": "email", "to": "invalid" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
