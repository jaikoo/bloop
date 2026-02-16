use moka::sync::Cache;
use std::time::Duration;

/// Simple cache wrapper keyed on `"{endpoint}:{project_id}:{hours}:{limit}"`.
/// Stores serialized JSON strings with a configurable TTL.
pub struct AnalyticsCache {
    inner: Cache<String, String>,
}

impl AnalyticsCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            inner: Cache::builder()
                .time_to_live(Duration::from_secs(ttl_secs))
                .max_capacity(256)
                .build(),
        }
    }

    pub fn cache_key(
        endpoint: &str,
        project_id: &Option<String>,
        hours: i64,
        limit: i64,
        environment: &Option<String>,
    ) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            endpoint,
            project_id.as_deref().unwrap_or("all"),
            hours,
            limit,
            environment.as_deref().unwrap_or("all"),
        )
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.inner.get(key)
    }

    pub fn insert(&self, key: String, value: String) {
        self.inner.insert(key, value);
    }
}
