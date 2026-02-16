use rusqlite::Connection;

const MIGRATION_001: &str = include_str!("../../migrations/001_initial.sql");
const MIGRATION_002: &str = include_str!("../../migrations/002_webauthn.sql");
const MIGRATION_003: &str = include_str!("../../migrations/003_multi_user.sql");
const MIGRATION_004: &str = include_str!("../../migrations/004_projects.sql");
const MIGRATION_005: &str = include_str!("../../migrations/005_timeseries.sql");
const MIGRATION_006: &str = include_str!("../../migrations/006_alert_channels.sql");
const MIGRATION_007: &str = include_str!("../../migrations/007_sourcemaps.sql");
const MIGRATION_008: &str = include_str!("../../migrations/008_index_optimization.sql");
const MIGRATION_009: &str = include_str!("../../migrations/009_bearer_tokens.sql");
const MIGRATION_010: &str = include_str!("../../migrations/010_hash_sessions.sql");
const MIGRATION_011: &str = include_str!("../../migrations/011_retention_settings.sql");
const MIGRATION_012: &str = include_str!("../../migrations/012_llm_tracing.sql");
const MIGRATION_013: &str = include_str!("../../migrations/013_llm_alert_rules.sql");
const MIGRATION_014: &str = include_str!("../../migrations/014_llm_search.sql");
const MIGRATION_015: &str = include_str!("../../migrations/015_llm_scores.sql");
const MIGRATION_016: &str = include_str!("../../migrations/016_llm_prompt_fields.sql");
const MIGRATION_017: &str = include_str!("../../migrations/017_pricing_overrides.sql");
const MIGRATION_018: &str = include_str!("../../migrations/018_llm_sessions.sql");
const MIGRATION_019: &str = include_str!("../../migrations/019_llm_feedback.sql");
const MIGRATION_020: &str = include_str!("../../migrations/020_llm_budgets.sql");

pub fn run_migrations(conn: &Connection) -> rusqlite::Result<()> {
    // Create migrations tracking table
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _migrations (
            id      INTEGER PRIMARY KEY,
            name    TEXT NOT NULL,
            applied INTEGER NOT NULL
        );",
    )?;

    let migrations: &[(i64, &str, &str)] = &[
        (1, "001_initial", MIGRATION_001),
        (2, "002_webauthn", MIGRATION_002),
        (3, "003_multi_user", MIGRATION_003),
        // Migration 4 has a Rust-assisted step; SQL is applied first, then data migration
        (4, "004_projects", MIGRATION_004),
        (5, "005_timeseries", MIGRATION_005),
        (6, "006_alert_channels", MIGRATION_006),
        (7, "007_sourcemaps", MIGRATION_007),
        (8, "008_index_optimization", MIGRATION_008),
        (9, "009_bearer_tokens", MIGRATION_009),
        (10, "010_hash_sessions", MIGRATION_010),
        (11, "011_retention_settings", MIGRATION_011),
        (12, "012_llm_tracing", MIGRATION_012),
        (13, "013_llm_alert_rules", MIGRATION_013),
        (14, "014_llm_search", MIGRATION_014),
        (15, "015_llm_scores", MIGRATION_015),
        (16, "016_llm_prompt_fields", MIGRATION_016),
        (17, "017_pricing_overrides", MIGRATION_017),
        (18, "018_llm_sessions", MIGRATION_018),
        (19, "019_llm_feedback", MIGRATION_019),
        (20, "020_llm_budgets", MIGRATION_020),
    ];

    for &(id, name, sql) in migrations {
        let applied: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM _migrations WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !applied {
            tracing::info!(migration = name, "applying migration");
            conn.execute_batch(sql)?;

            // Rust-assisted step for migration 004
            if id == 4 {
                run_migration_004_data(conn)?;
            }

            conn.execute(
                "INSERT INTO _migrations (id, name, applied) VALUES (?1, ?2, unixepoch())",
                rusqlite::params![id, name],
            )?;
        }
    }

    Ok(())
}

/// Rust-assisted data migration for 004_projects:
/// 1. Create a default project using config hmac_secret (passed via env or hardcoded)
/// 2. Migrate data from error_aggregates → error_aggregates_v2
/// 3. Migrate data from alert_cooldowns → alert_cooldowns_v2
/// 4. Drop old tables and rename v2 tables
fn run_migration_004_data(conn: &Connection) -> rusqlite::Result<()> {
    let default_project_id = "default";
    let now = chrono::Utc::now().timestamp();

    // Read the hmac_secret from env or use the default project api_key
    let api_key = std::env::var("BLOOP__AUTH__HMAC_SECRET")
        .or_else(|_| std::env::var("BLOOP_DEFAULT_API_KEY"))
        .unwrap_or_else(|_| {
            // Try to read from the projects table in case it was already set
            // Fall back to a generated UUID
            uuid::Uuid::new_v4().to_string()
        });

    // Insert default project (idempotent via INSERT OR IGNORE)
    conn.execute(
        "INSERT OR IGNORE INTO projects (id, name, slug, api_key, created_at, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
        rusqlite::params![default_project_id, "Default", "default", api_key, now],
    )?;

    // Set project_id on existing rows
    conn.execute(
        "UPDATE raw_events SET project_id = ?1 WHERE project_id IS NULL",
        rusqlite::params![default_project_id],
    )?;
    conn.execute(
        "UPDATE sample_occurrences SET project_id = ?1 WHERE project_id IS NULL",
        rusqlite::params![default_project_id],
    )?;
    conn.execute(
        "UPDATE alert_rules SET project_id = ?1 WHERE project_id IS NULL",
        rusqlite::params![default_project_id],
    )?;

    // Migrate error_aggregates → error_aggregates_v2
    conn.execute(
        "INSERT INTO error_aggregates_v2
            (project_id, fingerprint, release, environment, total_count,
             first_seen, last_seen, error_type, message, source,
             route_or_procedure, screen, status)
         SELECT ?1, fingerprint, release, environment, total_count,
                first_seen, last_seen, error_type, message, source,
                route_or_procedure, screen, status
         FROM error_aggregates",
        rusqlite::params![default_project_id],
    )?;

    // Migrate alert_cooldowns → alert_cooldowns_v2
    conn.execute(
        "INSERT INTO alert_cooldowns_v2 (project_id, rule_id, fingerprint, last_fired)
         SELECT ?1, rule_id, fingerprint, last_fired
         FROM alert_cooldowns",
        rusqlite::params![default_project_id],
    )?;

    // Drop old tables and rename v2
    // Must disable foreign keys temporarily for DROP
    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    conn.execute_batch("DROP TABLE IF EXISTS error_aggregates;")?;
    conn.execute_batch("ALTER TABLE error_aggregates_v2 RENAME TO error_aggregates;")?;
    conn.execute_batch("DROP TABLE IF EXISTS alert_cooldowns;")?;
    conn.execute_batch("ALTER TABLE alert_cooldowns_v2 RENAME TO alert_cooldowns;")?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    // Recreate indexes on the new error_aggregates table
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_agg_project_release_count
            ON error_aggregates(project_id, release, total_count DESC);
         CREATE INDEX IF NOT EXISTS idx_agg_project_last_seen
            ON error_aggregates(project_id, last_seen DESC);
         CREATE INDEX IF NOT EXISTS idx_agg_project_unresolved
            ON error_aggregates(project_id, status, last_seen DESC)
            WHERE status = 'unresolved';",
    )?;

    tracing::info!("migration 004: data migration complete, default project created");
    Ok(())
}
