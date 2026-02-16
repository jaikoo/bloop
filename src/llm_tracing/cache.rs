use moka::sync::Cache;
use std::time::Duration;

/// Simple cache keyed on `"{endpoint}:{project_id}:{hours}:{limit}"`.
pub struct LlmCache {
    inner: Cache<String, String>,
}

impl LlmCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            inner: Cache::builder()
                .time_to_live(Duration::from_secs(ttl_secs))
                .max_capacity(256)
                .build(),
        }
    }

    #[allow(dead_code)]
    pub fn cache_key(
        endpoint: &str,
        project_id: &Option<String>,
        hours: i64,
        limit: i64,
        extra: &Option<String>,
    ) -> String {
        format!(
            "llm:{}:{}:{}:{}:{}",
            endpoint,
            project_id.as_deref().unwrap_or("all"),
            hours,
            limit,
            extra.as_deref().unwrap_or("all"),
        )
    }

    #[allow(dead_code)]
    pub fn get(&self, key: &str) -> Option<String> {
        self.inner.get(key)
    }

    #[allow(dead_code)]
    pub fn insert(&self, key: String, value: String) {
        self.inner.insert(key, value);
    }
}
