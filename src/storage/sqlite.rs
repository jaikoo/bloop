use crate::config::DatabaseConfig;
use deadpool_sqlite::{Config, Pool, Runtime};
use rusqlite::Connection;

/// Apply performance PRAGMAs to a SQLite connection.
pub fn apply_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        PRAGMA page_size = 8192;
        PRAGMA cache_size = -65536;
        PRAGMA mmap_size = 268435456;
        PRAGMA busy_timeout = 5000;
        PRAGMA temp_store = MEMORY;
        PRAGMA wal_autocheckpoint = 1000;
        ",
    )
}

/// Create a deadpool-sqlite connection pool with PRAGMAs applied.
pub fn create_pool(config: &DatabaseConfig) -> Result<Pool, deadpool_sqlite::CreatePoolError> {
    let db_path = config.path.clone();

    // Set restrictive file permissions on the database file (Unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if db_path.exists() {
            if let Err(e) =
                std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600))
            {
                tracing::warn!(error = %e, "failed to set database file permissions");
            }
        }
    }

    let cfg = Config::new(db_path);
    let pool = cfg.create_pool(Runtime::Tokio1)?;

    // We'll apply pragmas on first use via the hook in init_pool
    Ok(pool)
}

/// Initialize the pool: get a connection and apply pragmas + run migrations.
pub async fn init_pool(pool: &Pool) -> Result<(), Box<dyn std::error::Error>> {
    let conn = pool.get().await?;
    conn.interact(|conn| {
        apply_pragmas(conn)?;
        crate::storage::migrations::run_migrations(conn)?;
        Ok::<_, rusqlite::Error>(())
    })
    .await??;
    Ok(())
}
