pub mod alerts;
pub mod budget;
pub mod cache;
pub mod conn;
pub mod feedback;
pub mod handler;
pub mod pricing;
pub mod pricing_handler;
pub mod query;
pub mod query_handler;
pub mod scores;
pub mod settings;
pub mod types;
pub mod worker;
pub mod writer;

use crate::config::LlmTracingConfig;
use deadpool_sqlite::Pool;
use std::sync::Arc;
use tokio::sync::mpsc;

use types::ProcessedTrace;

/// Shared state for LLM tracing ingest endpoints.
pub struct LlmIngestState {
    pub config: LlmTracingConfig,
    pub tx: mpsc::Sender<ProcessedTrace>,
    pub pool: Pool,
    pub settings_cache: Arc<settings::SettingsCache>,
    pub pricing: Option<Arc<pricing::PricingTable>>,
}

/// Shared state for LLM tracing query endpoints (DuckDB).
pub struct LlmQueryState {
    pub conn: conn::LlmDuckDbConn,
    #[allow(dead_code)]
    pub cache: cache::LlmCache,
    pub pool: Pool,
    pub settings_cache: Arc<settings::SettingsCache>,
}

impl LlmQueryState {
    pub fn new(
        db_path: &std::path::Path,
        cache_ttl_secs: u64,
        pool: Pool,
        settings_cache: Arc<settings::SettingsCache>,
    ) -> Result<Self, String> {
        let conn = conn::LlmDuckDbConn::new(db_path)?;
        let cache = cache::LlmCache::new(cache_ttl_secs);
        Ok(Self {
            conn,
            cache,
            pool,
            settings_cache,
        })
    }
}
