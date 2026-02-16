use duckdb::Connection;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Wraps a single in-memory DuckDB connection that attaches a SQLite file read-only
/// via the `sqlite_scanner` extension.
pub struct DuckDbConn {
    conn: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    db_path: String,
}

impl DuckDbConn {
    /// Create a new in-memory DuckDB connection and attach the SQLite database.
    pub fn new(db_path: &Path) -> Result<Self, String> {
        let db_path_str = db_path
            .to_str()
            .ok_or_else(|| "invalid database path".to_string())?
            .to_string();

        let conn = Connection::open_in_memory()
            .map_err(|e| format!("failed to open DuckDB in-memory: {e}"))?;

        // Point DuckDB extension directory to a writable location (the home directory
        // may not exist in containers, causing INSTALL to fail).
        let ext_dir = std::env::var("DUCKDB_EXTENSION_DIR").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("duckdb_ext")
                .to_string_lossy()
                .into_owned()
        });
        let set_dir = format!(
            "SET extension_directory = '{}'",
            ext_dir.replace('\'', "''")
        );
        conn.execute_batch(&set_dir)
            .map_err(|e| format!("failed to set extension_directory: {e}"))?;

        // Load sqlite_scanner and attach the SQLite database read-only
        conn.execute_batch("INSTALL sqlite_scanner; LOAD sqlite_scanner;")
            .map_err(|e| format!("failed to load sqlite_scanner: {e}"))?;

        let attach_sql = format!(
            "ATTACH '{}' AS bloop (TYPE SQLITE, READ_ONLY)",
            db_path_str.replace('\'', "''")
        );
        conn.execute_batch(&attach_sql)
            .map_err(|e| format!("failed to attach SQLite database: {e}"))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: db_path_str,
        })
    }

    /// Execute a query via `spawn_blocking` to avoid blocking the async runtime.
    /// The closure receives a `&Connection` and returns a result.
    pub async fn query<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&Connection) -> Result<T, duckdb::Error> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::task::spawn_blocking(move || {
                let guard = conn.blocking_lock();
                f(&guard).map_err(|e| format!("DuckDB query error: {e}"))
            }),
        )
        .await
        .map_err(|_| "DuckDB query timed out after 30s".to_string())?
        .map_err(|e| format!("spawn_blocking join error: {e}"))?;
        result
    }

    /// Reconnect by detaching and re-attaching the SQLite database.
    #[allow(dead_code)]
    pub async fn reconnect(&self) -> Result<(), String> {
        let conn = self.conn.clone();
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let guard = conn.blocking_lock();
            guard
                .execute_batch("DETACH bloop")
                .map_err(|e| format!("failed to detach: {e}"))?;
            let attach_sql = format!(
                "ATTACH '{}' AS bloop (TYPE SQLITE, READ_ONLY)",
                db_path.replace('\'', "''")
            );
            guard
                .execute_batch(&attach_sql)
                .map_err(|e| format!("failed to re-attach: {e}"))
        })
        .await
        .map_err(|e| format!("spawn_blocking join error: {e}"))?
    }
}
