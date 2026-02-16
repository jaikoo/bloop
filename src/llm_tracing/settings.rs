use crate::llm_tracing::types::ContentStorage;
use deadpool_sqlite::Pool;
use moka::sync::Cache;
use rusqlite::params;
use std::time::Duration;

/// In-memory cache for per-project content storage settings.
pub struct SettingsCache {
    inner: Cache<String, ContentStorage>,
    pool: Pool,
}

impl SettingsCache {
    pub fn new(pool: Pool, ttl_secs: u64) -> Self {
        Self {
            inner: Cache::builder()
                .time_to_live(Duration::from_secs(ttl_secs))
                .max_capacity(1024)
                .build(),
            pool,
        }
    }

    /// Get cached content storage policy for a project.
    pub fn get(&self, project_id: &str) -> Option<ContentStorage> {
        self.inner.get(project_id)
    }

    /// Load setting from DB and populate cache. Returns the policy.
    pub async fn load(
        &self,
        project_id: &str,
    ) -> Result<ContentStorage, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.pool.get().await?;
        let pid = project_id.to_string();
        let result = conn
            .interact(move |conn| {
                conn.query_row(
                    "SELECT content_storage FROM llm_project_settings WHERE project_id = ?1",
                    params![pid],
                    |row| {
                        let val: String = row.get(0)?;
                        Ok(ContentStorage::from_str_loose(&val))
                    },
                )
            })
            .await
            .map_err(|e| format!("interact error: {e}"))?;

        match result {
            Ok(policy) => {
                self.inner.insert(project_id.to_string(), policy.clone());
                Ok(policy)
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(ContentStorage::None),
            Err(e) => Err(Box::new(e)),
        }
    }

    /// Update project settings and refresh cache.
    pub async fn update(
        &self,
        project_id: &str,
        policy: ContentStorage,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.pool.get().await?;
        let pid = project_id.to_string();
        let policy_str = policy.as_str().to_string();
        let now = chrono::Utc::now().timestamp_millis();

        conn.interact(move |conn| {
            conn.execute(
                "INSERT INTO llm_project_settings (project_id, content_storage, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (project_id) DO UPDATE SET
                    content_storage = excluded.content_storage,
                    updated_at = excluded.updated_at",
                params![pid, policy_str, now],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .await
        .map_err(|e| format!("interact error: {e}"))??;

        self.inner.insert(project_id.to_string(), policy);
        Ok(())
    }
}
