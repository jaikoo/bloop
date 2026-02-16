pub mod cache;
pub mod conn;
pub mod handler;
pub mod queries;
pub mod types;

use crate::config::AnalyticsConfig;
use cache::AnalyticsCache;
use conn::DuckDbConn;
use std::path::Path;

/// Shared state for analytics endpoints.
pub struct AnalyticsState {
    pub conn: DuckDbConn,
    pub cache: AnalyticsCache,
    pub config: AnalyticsConfig,
}

impl AnalyticsState {
    pub fn new(db_path: &Path, config: AnalyticsConfig) -> Result<Self, String> {
        let conn = DuckDbConn::new(db_path)?;
        let cache = AnalyticsCache::new(config.cache_ttl_secs);
        Ok(Self {
            conn,
            cache,
            config,
        })
    }
}
