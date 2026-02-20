mod alert;
#[cfg(feature = "analytics")]
mod analytics;
mod auth;
mod config;
mod error;
mod fingerprint;
mod ingest;
#[cfg(feature = "llm-tracing")]
mod llm_tracing;
mod pipeline;
mod project;
mod query;
mod sourcemap;
mod storage;
mod types;

use crate::alert::dispatch::AlertDispatcher;
use crate::auth::bearer::BearerTokenCache;
use crate::auth::state::AuthState;
use crate::auth::token_routes::TokenState;
use crate::config::AppConfig;
use crate::ingest::auth::{HmacSecret, ProjectKeyCache};
use crate::ingest::handler::{self, IngestState};
use crate::pipeline::aggregator;
use crate::query::handler as query_handler;
use crate::query::handler::QueryState;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{delete, get, post, put};
use axum::Router;
use clap::Parser;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::mpsc;
use tower_governor::{
    governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor, GovernorLayer,
};
use tower_http::cors::{AllowOrigin, CorsLayer};

#[derive(Parser)]
#[command(name = "bloop", about = "Self-hosted error observability service")]
struct Cli {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "bloop=info,tower_http=info".into()),
        )
        .json()
        .init();

    let cli = Cli::parse();
    let config = AppConfig::load(Some(&cli.config))?;

    if let Err(msg) = config.validate() {
        eprintln!("Configuration error: {msg}");
        return Err(msg.into());
    }

    tracing::info!(
        host = %config.server.host,
        port = config.server.port,
        db = %config.database.path.display(),
        "starting bloop"
    );

    // Setup SQLite pool
    let pool = storage::sqlite::create_pool(&config.database)?;
    storage::sqlite::init_pool(&pool).await?;
    tracing::info!("database initialized");

    // Setup MPSC channel
    let (tx, rx) = mpsc::channel(config.ingest.channel_capacity);

    // Setup in-memory aggregator
    let agg = aggregator::create_aggregator();

    // Setup alert dispatcher
    let slack_url = std::env::var("BLOOP_SLACK_WEBHOOK_URL").ok();
    let generic_url = std::env::var("BLOOP_WEBHOOK_URL").ok();
    let smtp_config = if config.smtp.enabled {
        Some(config.smtp.clone())
    } else {
        None
    };
    let dispatcher = Arc::new(AlertDispatcher::new(slack_url, generic_url, smtp_config));

    // New fingerprint channel for alerting
    let (new_fp_tx, new_fp_rx) = mpsc::unbounded_channel();

    // Spawn pipeline worker
    let worker_pool = pool.clone();
    let worker_agg = agg.clone();
    let pipeline_config = config.pipeline.clone();
    let worker_handle = tokio::spawn(async move {
        pipeline::worker::run_worker(rx, worker_pool, worker_agg, pipeline_config, new_fp_tx).await;
    });

    // Spawn alert evaluator
    let alert_pool = pool.clone();
    let alert_dispatcher = dispatcher.clone();
    let cooldown = config.alerting.cooldown_secs;
    tokio::spawn(async move {
        alert::rules::alert_evaluator(new_fp_rx, alert_pool, alert_dispatcher, cooldown).await;
    });

    // Spawn LLM alert evaluator (optional, feature-gated)
    #[cfg(feature = "llm-tracing")]
    if config.llm_tracing.enabled {
        let llm_alert_pool = pool.clone();
        let llm_alert_dispatcher = dispatcher.clone();
        let llm_cooldown = config.alerting.cooldown_secs;
        tokio::spawn(async move {
            llm_tracing::alerts::llm_alert_evaluator(
                llm_alert_pool,
                llm_alert_dispatcher,
                llm_cooldown,
            )
            .await;
        });
    }

    // Spawn retention background task
    let retention_pool = pool.clone();
    let retention_days = config.retention.raw_events_days;
    let retention_interval = config.retention.prune_interval_secs;
    tokio::spawn(async move {
        storage::retention::retention_loop(retention_pool, retention_days, retention_interval)
            .await;
    });

    // ── LLM Tracing pipeline (optional, feature-gated) ──
    #[cfg(feature = "llm-tracing")]
    let (llm_tx, llm_worker_handle) = if config.llm_tracing.enabled {
        let (llm_tx, llm_rx) = mpsc::channel::<llm_tracing::types::ProcessedTrace>(
            config.llm_tracing.channel_capacity,
        );
        let llm_worker_pool = pool.clone();
        let llm_config = config.llm_tracing.clone();
        let handle = tokio::spawn(async move {
            llm_tracing::worker::run_worker(llm_rx, llm_worker_pool, llm_config).await;
        });
        tracing::info!("llm tracing pipeline enabled");
        (Some(llm_tx), Some(handle))
    } else {
        (None, None)
    };

    // Setup auth state
    let auth_state = Arc::new(
        AuthState::new(&config.auth, pool.clone()).expect("failed to initialize WebAuthn"),
    );
    tracing::info!(rp_id = %config.auth.rp_id, "webauthn initialized");

    // Spawn session cleanup
    let cleanup_pool = pool.clone();
    tokio::spawn(async move {
        auth::session::session_cleanup_loop(cleanup_pool).await;
    });

    // Build shared state
    let ingest_state = Arc::new(IngestState {
        config: config.ingest.clone(),
        tx: tx.clone(),
    });

    let query_state = Arc::new(QueryState {
        pool: pool.clone(),
        aggregator: agg.clone(),
        channel_capacity: config.ingest.channel_capacity,
        channel_tx: tx.clone(),
    });

    // Setup ProjectKeyCache for ingest auth
    let legacy_secret = if config.auth.hmac_secret.is_empty() {
        None
    } else {
        Some(config.auth.hmac_secret.clone())
    };
    let project_key_cache = Arc::new(ProjectKeyCache::new(pool.clone(), legacy_secret.clone()));

    // Legacy HmacSecret for backward compat
    let hmac_secret = HmacSecret(config.auth.hmac_secret.clone());

    // Alert CRUD state (shares pool + dispatcher)
    let alert_state = Arc::new(AlertState {
        pool: pool.clone(),
        dispatcher: dispatcher.clone(),
    });

    // Pool for project routes
    let project_pool = Arc::new(pool.clone());

    // Sourcemap state
    let sourcemap_state = Arc::new(sourcemap::SourceMapState { pool: pool.clone() });

    // Bearer token cache for API token auth
    let bearer_cache = Arc::new(BearerTokenCache::new(pool.clone()));

    // Token CRUD state
    let token_state = Arc::new(TokenState {
        pool: pool.clone(),
        cache: bearer_cache.clone(),
    });

    // Retention API state
    let retention_state = Arc::new(storage::retention_api::RetentionState {
        pool: pool.clone(),
        default_raw_events_days: config.retention.raw_events_days,
    });

    // ── LLM Tracing states (optional, feature-gated) ──
    #[cfg(feature = "llm-tracing")]
    let llm_settings_cache = Arc::new(llm_tracing::settings::SettingsCache::new(
        pool.clone(),
        config.llm_tracing.cache_ttl_secs,
    ));

    // Init pricing table (bundled data + DB overrides + background refresh)
    #[cfg(feature = "llm-tracing")]
    let llm_pricing = if config.llm_tracing.enabled {
        let pricing = Arc::new(llm_tracing::pricing::PricingTable::new());
        // Load overrides from DB
        if let Err(e) = llm_tracing::pricing_handler::load_overrides_from_db(&pool, &pricing).await
        {
            tracing::warn!(error = %e, "failed to load pricing overrides from database");
        }
        // Spawn background refresh
        let refresh_pricing = pricing.clone();
        let refresh_interval = config.llm_tracing.pricing_refresh_interval_secs;
        let refresh_url = config.llm_tracing.pricing_url.clone();
        tokio::spawn(async move {
            llm_tracing::pricing::pricing_refresh_loop(
                refresh_pricing,
                refresh_interval,
                refresh_url,
            )
            .await;
        });
        Some(pricing)
    } else {
        None
    };

    #[cfg(feature = "llm-tracing")]
    let llm_ingest_state = llm_tx.as_ref().map(|tx| {
        Arc::new(llm_tracing::LlmIngestState {
            config: config.llm_tracing.clone(),
            tx: tx.clone(),
            pool: pool.clone(),
            settings_cache: llm_settings_cache.clone(),
            pricing: llm_pricing.clone(),
        })
    });

    #[cfg(feature = "llm-tracing")]
    let llm_query_state = if config.llm_tracing.enabled {
        match llm_tracing::LlmQueryState::new(
            &config.database.path,
            config.llm_tracing.cache_ttl_secs,
            pool.clone(),
            llm_settings_cache.clone(),
        ) {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                tracing::warn!(error = %e, "llm tracing query engine failed to initialize");
                None
            }
        }
    } else {
        None
    };

    // Pool extension for session middleware
    let session_pool = Arc::new(pool.clone());

    // Rate limiter for auth routes (stricter, configurable)
    let auth_governor_conf = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_second(config.rate_limit.auth_per_second)
        .burst_size(config.rate_limit.auth_burst_size)
        .finish()
        .expect("failed to build auth rate limiter config");

    // ── Auth routes (public, rate-limited) ──
    let auth_routes = Router::new()
        .route("/auth/status", get(auth::webauthn::status))
        .route("/auth/login", get(auth::webauthn::login_page))
        .route("/auth/register/start", post(auth::webauthn::register_start))
        .route(
            "/auth/register/finish",
            post(auth::webauthn::register_finish),
        )
        .route("/auth/login/start", post(auth::webauthn::login_start))
        .route("/auth/login/finish", post(auth::webauthn::login_finish))
        .route("/auth/logout", post(auth::webauthn::logout))
        .route(
            "/auth/invite/register/start",
            post(auth::webauthn::invite_register_start),
        )
        .route(
            "/auth/invite/register/finish",
            post(auth::webauthn::invite_register_finish),
        )
        .layer(GovernorLayer::new(auth_governor_conf))
        .with_state(auth_state.clone());

    // ── Admin routes (session-protected, admin checked in handlers) ──
    let admin_routes = Router::new()
        .route("/v1/admin/users", get(auth::webauthn::list_users))
        .route(
            "/v1/admin/users/{user_id}",
            delete(auth::webauthn::delete_user),
        )
        .route(
            "/v1/admin/users/{user_id}/role",
            put(auth::webauthn::update_user_role),
        )
        .route("/v1/admin/invites", get(auth::webauthn::list_invites))
        .route("/v1/admin/invites", post(auth::webauthn::create_invite))
        .route("/v1/admin/reset", post(auth::webauthn::admin_reset))
        .layer(middleware::from_fn(auth::session::require_session_api))
        .layer(axum::Extension(session_pool.clone()))
        .with_state(auth_state);

    // ── Admin retention routes (session-protected) ──
    let retention_routes = Router::new()
        .route(
            "/v1/admin/retention",
            get(storage::retention_api::get_retention),
        )
        .route(
            "/v1/admin/retention",
            put(storage::retention_api::update_retention),
        )
        .route(
            "/v1/admin/retention/purge",
            post(storage::retention_api::purge_now),
        )
        .layer(middleware::from_fn(auth::session::require_session_api))
        .layer(axum::Extension(session_pool.clone()))
        .with_state(retention_state);

    // Rate limiter for ingest routes
    let governor_conf = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_second(config.rate_limit.per_second)
        .burst_size(config.rate_limit.burst_size)
        .finish()
        .expect("failed to build rate limiter config");

    // HMAC body limit must match the configured max payload size
    let hmac_body_limit = ingest::auth::HmacBodyLimit(config.ingest.max_payload_bytes);

    // Rate limiter for LLM tracing ingest routes (same config as error ingest)
    #[cfg(feature = "llm-tracing")]
    let llm_governor_conf = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_second(config.rate_limit.per_second)
        .burst_size(config.rate_limit.burst_size)
        .finish()
        .expect("failed to build llm rate limiter config");

    #[cfg(feature = "llm-tracing")]
    let llm_proxy_governor_conf = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_second(config.rate_limit.per_second)
        .burst_size(config.rate_limit.burst_size)
        .finish()
        .expect("failed to build llm proxy rate limiter config");

    // Clone HMAC references for LLM tracing ingest (before they're moved)
    #[cfg(feature = "llm-tracing")]
    let project_key_cache_for_llm = project_key_cache.clone();
    #[cfg(feature = "llm-tracing")]
    let hmac_secret_for_llm = hmac_secret.clone();
    #[cfg(feature = "llm-tracing")]
    let hmac_body_limit_for_llm = hmac_body_limit.clone();

    // ── Ingest routes (HMAC auth with project key support) ──
    let ingest_routes = Router::new()
        .route("/v1/ingest", post(handler::ingest_single))
        .route("/v1/ingest/batch", post(handler::ingest_batch))
        .layer(DefaultBodyLimit::max(config.ingest.max_payload_bytes))
        .layer(middleware::from_fn(ingest::auth::hmac_auth))
        .layer(axum::Extension(project_key_cache))
        .layer(axum::Extension(hmac_secret))
        .layer(axum::Extension(hmac_body_limit))
        .layer(GovernorLayer::new(governor_conf))
        .with_state(ingest_state);

    // ── Health route (public) ──
    let health_route = Router::new()
        .route("/health", get(query_handler::health))
        .with_state(query_state.clone());

    // ── Protected query routes (bearer token or session auth) ──
    let query_routes = Router::new()
        .route("/v1/errors", get(query_handler::list_errors))
        .route(
            "/v1/errors/{fingerprint}",
            get(query_handler::get_error_detail),
        )
        .route(
            "/v1/errors/{fingerprint}/occurrences",
            get(query_handler::get_occurrences),
        )
        .route(
            "/v1/errors/{fingerprint}/resolve",
            post(query_handler::resolve_error),
        )
        .route(
            "/v1/errors/{fingerprint}/ignore",
            post(query_handler::ignore_error),
        )
        .route(
            "/v1/errors/{fingerprint}/mute",
            post(query_handler::mute_error),
        )
        .route(
            "/v1/errors/{fingerprint}/unresolve",
            post(query_handler::unresolve_error),
        )
        .route(
            "/v1/errors/{fingerprint}/trend",
            get(query_handler::error_trend),
        )
        .route(
            "/v1/errors/{fingerprint}/history",
            get(query_handler::error_history),
        )
        .route(
            "/v1/releases/{release}/errors",
            get(query_handler::release_errors),
        )
        .route("/v1/trends", get(query_handler::global_trends))
        .route("/v1/stats", get(query_handler::stats))
        .layer(middleware::from_fn(auth::bearer::require_bearer_or_session))
        .layer(axum::Extension(bearer_cache.clone()))
        .layer(axum::Extension(session_pool.clone()))
        .with_state(query_state);

    // ── Protected alert routes (bearer token or session auth) ──
    let alert_routes = Router::new()
        .route("/v1/alerts", get(list_alerts_handler))
        .route("/v1/alerts", post(create_alert_handler))
        .route("/v1/alerts/{id}", put(update_alert_handler))
        .route("/v1/alerts/{id}", delete(delete_alert_handler))
        .route("/v1/alerts/{id}/channels", get(list_channels_handler))
        .route("/v1/alerts/{id}/channels", post(add_channel_handler))
        .route(
            "/v1/alerts/{id}/channels/{cid}",
            delete(delete_channel_handler),
        )
        .route("/v1/alerts/{id}/test", post(test_alert_handler))
        .layer(middleware::from_fn(auth::bearer::require_bearer_or_session))
        .layer(axum::Extension(bearer_cache.clone()))
        .layer(axum::Extension(session_pool.clone()))
        .with_state(alert_state);

    // ── Protected project routes (session auth) ──
    let project_routes = Router::new()
        .route("/v1/projects", get(project::list_projects))
        .route("/v1/projects", post(project::create_project))
        .route("/v1/projects/{slug}", get(project::get_project))
        .route("/v1/projects/{slug}", put(project::update_project))
        .route("/v1/projects/{slug}", delete(project::delete_project))
        .route("/v1/projects/{slug}/rotate-key", post(project::rotate_key))
        .layer(middleware::from_fn(auth::session::require_session_api))
        .layer(axum::Extension(session_pool.clone()))
        .with_state(project_pool);

    // ── Protected sourcemap routes (bearer token or session auth) ──
    let sourcemap_routes = Router::new()
        .route(
            "/v1/projects/{slug}/sourcemaps",
            get(sourcemap::list_sourcemaps),
        )
        .route(
            "/v1/projects/{slug}/sourcemaps",
            post(sourcemap::upload_sourcemap),
        )
        .route(
            "/v1/projects/{slug}/sourcemaps/{id}",
            delete(sourcemap::delete_sourcemap),
        )
        .layer(DefaultBodyLimit::max(5 * 1024 * 1024)) // 5MB limit for sourcemap uploads
        .layer(middleware::from_fn(auth::bearer::require_bearer_or_session))
        .layer(axum::Extension(bearer_cache.clone()))
        .layer(axum::Extension(session_pool.clone()))
        .with_state(sourcemap_state);

    // ── Token CRUD routes (session-only, manage tokens from dashboard) ──
    let token_routes = Router::new()
        .route("/v1/tokens", post(auth::token_routes::create_token))
        .route("/v1/tokens", get(auth::token_routes::list_tokens))
        .route("/v1/tokens/{id}", delete(auth::token_routes::revoke_token))
        .layer(middleware::from_fn(auth::session::require_session_api))
        .layer(axum::Extension(session_pool.clone()))
        .with_state(token_state);

    // ── Dashboard (session auth with redirect) ──
    let dashboard_route = Router::new()
        .route("/", get(dashboard_handler))
        .layer(middleware::from_fn(auth::session::require_session_browser))
        .layer(axum::Extension(session_pool.clone()));

    // CORS for API/dashboard: restrict to configured origin with credentials
    let api_cors = CorsLayer::new()
        .allow_origin(AllowOrigin::exact(
            config
                .auth
                .rp_origin
                .parse()
                .expect("rp_origin must be a valid header value"),
        ))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::COOKIE,
            axum::http::header::AUTHORIZATION,
        ])
        .allow_credentials(true);

    // CORS for ingest: allow any origin (SDKs report from various domains)
    let ingest_cors = CorsLayer::new()
        .allow_origin(AllowOrigin::any())
        .allow_methods([axum::http::Method::POST, axum::http::Method::OPTIONS])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            "X-Signature".parse().unwrap(),
            "X-Project-Key".parse().unwrap(),
        ]);

    // ── Analytics routes (optional, feature-gated) ──
    #[cfg(feature = "analytics")]
    let analytics_router = if config.analytics.enabled {
        match analytics::AnalyticsState::new(&config.database.path, config.analytics.clone()) {
            Ok(analytics_state) => {
                let analytics_state = Arc::new(analytics_state);
                tracing::info!("analytics engine enabled (DuckDB)");
                Some(
                    Router::new()
                        .route(
                            "/v1/analytics/spikes",
                            get(analytics::handler::spike_detection),
                        )
                        .route("/v1/analytics/movers", get(analytics::handler::top_movers))
                        .route(
                            "/v1/analytics/correlations",
                            get(analytics::handler::error_correlation),
                        )
                        .route(
                            "/v1/analytics/releases",
                            get(analytics::handler::release_impact),
                        )
                        .route(
                            "/v1/analytics/environments",
                            get(analytics::handler::env_breakdown),
                        )
                        .layer(middleware::from_fn(auth::bearer::require_bearer_or_session))
                        .layer(axum::Extension(bearer_cache.clone()))
                        .layer(axum::Extension(session_pool.clone()))
                        .with_state(analytics_state),
                )
            }
            Err(e) => {
                tracing::warn!(error = %e, "analytics engine failed to initialize, running without analytics");
                None
            }
        }
    } else {
        None
    };

    // Apply API CORS to non-ingest routes, then merge ingest (which has its own CORS)
    #[allow(unused_mut)]
    let mut api_routes = Router::new()
        .merge(dashboard_route)
        .merge(auth_routes)
        .merge(admin_routes)
        .merge(health_route)
        .merge(query_routes)
        .merge(alert_routes)
        .merge(project_routes)
        .merge(sourcemap_routes)
        .merge(token_routes)
        .merge(retention_routes);

    #[cfg(feature = "analytics")]
    if let Some(analytics_router) = analytics_router {
        api_routes = api_routes.merge(analytics_router);
    }

    // ── LLM Tracing query routes (optional, feature-gated) ──
    #[cfg(feature = "llm-tracing")]
    if let Some(ref llm_qs) = llm_query_state {
        let llm_query_routes = Router::new()
            .route(
                "/v1/llm/overview",
                get(llm_tracing::query_handler::overview),
            )
            .route("/v1/llm/usage", get(llm_tracing::query_handler::usage))
            .route("/v1/llm/latency", get(llm_tracing::query_handler::latency))
            .route("/v1/llm/models", get(llm_tracing::query_handler::models))
            .route(
                "/v1/llm/traces",
                get(llm_tracing::query_handler::traces_list),
            )
            .route(
                "/v1/llm/traces/{id}",
                get(llm_tracing::query_handler::trace_detail),
            )
            .route("/v1/llm/search", get(llm_tracing::query_handler::search))
            .route("/v1/llm/prompts", get(llm_tracing::query_handler::prompts))
            .route(
                "/v1/llm/prompts/{name}/versions",
                get(llm_tracing::query_handler::prompt_versions),
            )
            .route(
                "/v1/llm/prompts/{name}/compare",
                get(llm_tracing::query_handler::prompt_compare),
            )
            .route(
                "/v1/llm/sessions",
                get(llm_tracing::query_handler::sessions_list),
            )
            .route(
                "/v1/llm/sessions/{session_id}",
                get(llm_tracing::query_handler::session_detail),
            )
            .route("/v1/llm/tools", get(llm_tracing::query_handler::tools))
            .route("/v1/llm/rag", get(llm_tracing::query_handler::rag))
            .route(
                "/v1/llm/settings",
                get(llm_tracing::query_handler::get_settings),
            )
            .route(
                "/v1/llm/settings",
                put(llm_tracing::query_handler::update_settings),
            )
            .layer(middleware::from_fn(auth::bearer::require_bearer_or_session))
            .layer(axum::Extension(bearer_cache.clone()))
            .layer(axum::Extension(session_pool.clone()))
            .with_state(llm_qs.clone());
        api_routes = api_routes.merge(llm_query_routes);

        let llm_export_routes = Router::new()
            .route(
                "/v1/llm/export/langsmith",
                get(llm_tracing::export::export_langsmith),
            )
            .route(
                "/v1/llm/export/otlp",
                post(llm_tracing::export::import_otlp),
            )
            .layer(middleware::from_fn(auth::bearer::require_bearer_or_session))
            .layer(axum::Extension(bearer_cache.clone()))
            .layer(axum::Extension(session_pool.clone()))
            .with_state(llm_qs.clone());
        api_routes = api_routes.merge(llm_export_routes);

        let llm_experiment_routes = Router::new()
            .route(
                "/v1/llm/experiments",
                get(llm_tracing::experiment::list_experiments)
                    .post(llm_tracing::experiment::create_experiment),
            )
            .route(
                "/v1/llm/experiments/{id}",
                get(llm_tracing::experiment::get_experiment),
            )
            .route(
                "/v1/llm/experiments/{id}/compare",
                get(llm_tracing::experiment::compare_variants),
            )
            .layer(middleware::from_fn(auth::bearer::require_bearer_or_session))
            .layer(axum::Extension(bearer_cache.clone()))
            .layer(axum::Extension(session_pool.clone()))
            .with_state(llm_qs.clone());
        api_routes = api_routes.merge(llm_experiment_routes);

        // Score routes use Arc<Pool> state (direct SQLite, no DuckDB)
        let llm_score_pool = Arc::new(pool.clone());
        let llm_score_routes = Router::new()
            .route(
                "/v1/llm/traces/{trace_id}/scores",
                post(llm_tracing::scores::post_score),
            )
            .route(
                "/v1/llm/traces/{trace_id}/scores",
                get(llm_tracing::scores::get_scores),
            )
            .route(
                "/v1/llm/scores/summary",
                get(llm_tracing::scores::get_score_summary),
            )
            .layer(middleware::from_fn(auth::bearer::require_bearer_or_session))
            .layer(axum::Extension(bearer_cache.clone()))
            .layer(axum::Extension(session_pool.clone()))
            .with_state(llm_score_pool);
        api_routes = api_routes.merge(llm_score_routes);

        // Feedback routes use Arc<Pool> state (direct SQLite, same pattern as scores)
        let llm_feedback_pool = Arc::new(pool.clone());
        let llm_feedback_routes = Router::new()
            .route(
                "/v1/llm/traces/{trace_id}/feedback",
                post(llm_tracing::feedback::post_feedback),
            )
            .route(
                "/v1/llm/traces/{trace_id}/feedback",
                get(llm_tracing::feedback::get_feedback),
            )
            .route(
                "/v1/llm/feedback/summary",
                get(llm_tracing::feedback::get_feedback_summary),
            )
            .layer(middleware::from_fn(auth::bearer::require_bearer_or_session))
            .layer(axum::Extension(bearer_cache.clone()))
            .layer(axum::Extension(session_pool.clone()))
            .with_state(llm_feedback_pool);
        api_routes = api_routes.merge(llm_feedback_routes);

        // Budget routes use Arc<Pool> state (direct SQLite)
        let llm_budget_pool = Arc::new(pool.clone());
        let llm_budget_routes = Router::new()
            .route("/v1/llm/budget", get(llm_tracing::budget::get_budget))
            .route("/v1/llm/budget", put(llm_tracing::budget::set_budget))
            .layer(middleware::from_fn(auth::bearer::require_bearer_or_session))
            .layer(axum::Extension(bearer_cache.clone()))
            .layer(axum::Extension(session_pool.clone()))
            .with_state(llm_budget_pool);
        api_routes = api_routes.merge(llm_budget_routes);

        // Pricing routes
        if let Some(ref pricing) = llm_pricing {
            let pricing_state = Arc::new(llm_tracing::pricing_handler::PricingState {
                pricing: pricing.clone(),
                pool: pool.clone(),
            });
            let pricing_routes = Router::new()
                .route(
                    "/v1/llm/pricing",
                    get(llm_tracing::pricing_handler::get_pricing),
                )
                .route(
                    "/v1/llm/pricing/overrides",
                    put(llm_tracing::pricing_handler::set_pricing_override),
                )
                .layer(middleware::from_fn(auth::bearer::require_bearer_or_session))
                .layer(axum::Extension(bearer_cache.clone()))
                .layer(axum::Extension(session_pool.clone()))
                .with_state(pricing_state);
            api_routes = api_routes.merge(pricing_routes);
        }
    }

    let api_routes = api_routes.layer(api_cors);

    // ── LLM Tracing ingest routes (optional, feature-gated, HMAC auth) ──
    #[cfg(feature = "llm-tracing")]
    let ingest_routes = if let Some(ref llm_is) = llm_ingest_state {
        let llm_ingest_routes = Router::new()
            .route("/v1/traces", post(llm_tracing::handler::ingest_trace))
            .route(
                "/v1/traces/batch",
                post(llm_tracing::handler::ingest_trace_batch),
            )
            .route(
                "/v1/traces/{trace_id}",
                put(llm_tracing::handler::update_trace),
            )
            .layer(DefaultBodyLimit::max(config.ingest.max_payload_bytes))
            .layer(middleware::from_fn(ingest::auth::hmac_auth))
            .layer(axum::Extension(project_key_cache_for_llm.clone()))
            .layer(axum::Extension(hmac_secret_for_llm.clone()))
            .layer(axum::Extension(hmac_body_limit_for_llm.clone()))
            .layer(GovernorLayer::new(llm_governor_conf))
            .with_state(llm_is.clone());

        let proxy_config = llm_tracing::proxy::ProxyConfig {
            enabled: true,
            providers: vec!["openai".to_string()],
            openai_base_url: "https://api.openai.com/v1".to_string(),
            anthropic_base_url: "https://api.anthropic.com/v1".to_string(),
            capture_prompts: true,
            capture_completions: true,
            capture_streaming: true,
        };
        let proxy_state = llm_tracing::proxy::ProxyState::from_ingest_state(llm_is, proxy_config);
        let llm_proxy_routes = Router::new()
            .route(
                "/v1/proxy/{provider}/{*path}",
                post(llm_tracing::proxy::proxy_handler),
            )
            .layer(DefaultBodyLimit::max(config.ingest.max_payload_bytes))
            .layer(GovernorLayer::new(llm_proxy_governor_conf))
            .with_state(Arc::new(proxy_state));

        ingest_routes
            .merge(llm_ingest_routes)
            .merge(llm_proxy_routes)
    } else {
        ingest_routes
    };

    let app = api_routes.merge(ingest_routes.layer(ingest_cors));

    // Start server
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!(addr = %addr, "listening");

    #[cfg(feature = "llm-tracing")]
    let llm_shutdown = llm_tx.map(|t| (t, llm_worker_handle.unwrap()));

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(
            tx,
            worker_handle,
            #[cfg(feature = "llm-tracing")]
            llm_shutdown,
        ))
        .await?;

    tracing::info!("shutdown complete");
    Ok(())
}

async fn shutdown_signal(
    tx: mpsc::Sender<types::ProcessedEvent>,
    worker_handle: tokio::task::JoinHandle<()>,
    #[cfg(feature = "llm-tracing")] llm_shutdown: Option<(
        mpsc::Sender<llm_tracing::types::ProcessedTrace>,
        tokio::task::JoinHandle<()>,
    )>,
) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received Ctrl+C"),
        _ = terminate => tracing::info!("received SIGTERM"),
    }

    tracing::info!("shutting down...");

    // Drop sender to signal error pipeline worker to drain
    drop(tx);

    // Drop LLM tracing sender and await its worker
    #[cfg(feature = "llm-tracing")]
    if let Some((llm_tx, llm_handle)) = llm_shutdown {
        drop(llm_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), llm_handle).await;
    }

    // Wait for error pipeline worker with timeout
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), worker_handle).await;
}

// Alert shared state
struct AlertState {
    pool: deadpool_sqlite::Pool,
    dispatcher: Arc<AlertDispatcher>,
}

// Alert CRUD route handlers (thin wrappers around rules module)

async fn list_alerts_handler(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
) -> error::AppResult<axum::Json<Vec<alert::rules::AlertRule>>> {
    let rules = alert::rules::list_alert_rules(&state.pool).await?;
    Ok(axum::Json(rules))
}

async fn create_alert_handler(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::Json(input): axum::Json<alert::rules::CreateAlertRule>,
) -> error::AppResult<axum::Json<alert::rules::AlertRule>> {
    let rule = alert::rules::create_alert_rule(&state.pool, input).await?;
    Ok(axum::Json(rule))
}

async fn update_alert_handler(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<alert::rules::UpdateAlertRule>,
) -> error::AppResult<axum::Json<alert::rules::AlertRule>> {
    let rule = alert::rules::update_alert_rule(&state.pool, id, input).await?;
    Ok(axum::Json(rule))
}

async fn delete_alert_handler(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> error::AppResult<axum::Json<serde_json::Value>> {
    alert::rules::delete_alert_rule(&state.pool, id).await?;
    Ok(axum::Json(serde_json::json!({ "deleted": id })))
}

async fn list_channels_handler(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> error::AppResult<axum::Json<Vec<alert::rules::AlertChannel>>> {
    let channels = alert::rules::list_channels(&state.pool, id).await?;
    Ok(axum::Json(channels))
}

async fn add_channel_handler(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<alert::rules::CreateAlertChannel>,
) -> error::AppResult<axum::Json<alert::rules::AlertChannel>> {
    let channel = alert::rules::add_channel(&state.pool, id, input).await?;
    Ok(axum::Json(channel))
}

async fn delete_channel_handler(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::extract::Path((id, cid)): axum::extract::Path<(i64, i64)>,
) -> error::AppResult<axum::Json<serde_json::Value>> {
    alert::rules::delete_channel(&state.pool, id, cid).await?;
    Ok(axum::Json(serde_json::json!({ "deleted": cid })))
}

async fn test_alert_handler(
    axum::extract::State(state): axum::extract::State<Arc<AlertState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> error::AppResult<axum::Json<serde_json::Value>> {
    alert::rules::test_alert(&state.pool, id, &state.dispatcher).await?;
    Ok(axum::Json(serde_json::json!({ "sent": true })))
}

// Dashboard - embedded HTML served at /
async fn dashboard_handler() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("dashboard.html"))
}
