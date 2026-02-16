/// Helper to create a temp DB pool with all migrations applied and a test user.
async fn setup_pool() -> deadpool_sqlite::Pool {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    let pool = {
        let cfg = deadpool_sqlite::Config::new(&db_path);
        cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap()
    };

    let conn = pool.get().await.unwrap();
    conn.interact(|conn| {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .unwrap();
        bloop::storage::migrations::run_migrations(conn).unwrap();

        // Insert test user (required FK for sessions and bearer_tokens)
        conn.execute(
            "INSERT OR IGNORE INTO webauthn_users (id, username, display_name, created_at)
             VALUES ('user-1', 'testuser', 'Test User', 0)",
            [],
        )
        .unwrap();
    })
    .await
    .unwrap();

    pool
}

// ═══════════════════════════════════════════════════════════════
// C-1: Session token hashing
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_session_token_is_hashed_in_db() {
    let pool = setup_pool().await;

    // Create a session
    let token = bloop::auth::session::create_session(&pool, "user-1", 3600)
        .await
        .expect("create_session should succeed");

    // The plaintext token should NOT appear in the DB
    let conn = pool.get().await.unwrap();
    let plaintext = token.clone();
    let found_plaintext = conn
        .interact(move |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE token = ?1",
                rusqlite::params![plaintext],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(found_plaintext, 0, "plaintext token should NOT be in DB");

    // The SHA-256 hash SHOULD appear in the DB
    let hash = bloop::auth::bearer::hash_token(&token);
    let found_hash = conn
        .interact(move |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE token = ?1",
                rusqlite::params![hash],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(found_hash, 1, "hashed token SHOULD be in DB");
}

#[tokio::test]
async fn test_session_delete_uses_hash() {
    let pool = setup_pool().await;

    let token = bloop::auth::session::create_session(&pool, "user-1", 3600)
        .await
        .expect("create_session should succeed");

    // Delete the session using the plaintext token (should hash internally)
    bloop::auth::session::delete_session(&pool, &token)
        .await
        .expect("delete_session should succeed");

    // Verify it's gone
    let hash = bloop::auth::bearer::hash_token(&token);
    let conn = pool.get().await.unwrap();
    let count = conn
        .interact(move |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE token = ?1",
                rusqlite::params![hash],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(count, 0, "session should be deleted");
}

// ═══════════════════════════════════════════════════════════════
// H-2: ProjectSummary does not leak api_key
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_project_summary_excludes_api_key() {
    let project = bloop::project::Project {
        id: "test-id".to_string(),
        name: "Test".to_string(),
        slug: "test".to_string(),
        api_key: "bloop_secret_key_12345".to_string(),
        created_at: 1700000000,
        created_by: Some("user-1".to_string()),
    };

    let summary: bloop::project::ProjectSummary = project.into();
    let json = serde_json::to_value(&summary).unwrap();

    assert!(
        json.get("api_key").is_none(),
        "ProjectSummary should NOT contain api_key"
    );
    assert_eq!(json["id"], "test-id");
    assert_eq!(json["name"], "Test");
    assert_eq!(json["slug"], "test");
}

// ═══════════════════════════════════════════════════════════════
// H-3: Bearer token cache expiry re-validation
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_expired_bearer_token_rejected() {
    let pool = setup_pool().await;

    // Insert a project for the token
    let conn = pool.get().await.unwrap();
    conn.interact(|conn| {
        conn.execute(
            "INSERT INTO projects (id, name, slug, api_key, created_at) VALUES ('proj-1', 'Test', 'test', 'key', 0)",
            [],
        ).unwrap();
    })
    .await
    .unwrap();

    // Insert a bearer token that expired 1 hour ago
    let plaintext = "bloop_tk_test_expired_token_1234567890ab";
    let hash = bloop::auth::bearer::hash_token(plaintext);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let expired_at = now - 3600; // expired 1 hour ago

    let h = hash.clone();
    conn.interact(move |conn| {
        conn.execute(
            "INSERT INTO bearer_tokens (id, name, token_hash, token_prefix, project_id, created_by, scopes, created_at, expires_at)
             VALUES ('tok-1', 'test', ?1, 'bloop_tk_test_', 'proj-1', 'user-1', '[\"errors:read\"]', ?2, ?3)",
            rusqlite::params![h, now, expired_at],
        ).unwrap();
    })
    .await
    .unwrap();

    // Create cache and try to resolve
    let cache = bloop::auth::bearer::BearerTokenCache::new(pool);
    let result = cache.resolve(plaintext).await;
    assert!(result.is_none(), "expired token should be rejected");
}

#[tokio::test]
async fn test_valid_bearer_token_accepted() {
    let pool = setup_pool().await;

    let conn = pool.get().await.unwrap();
    conn.interact(|conn| {
        conn.execute(
            "INSERT INTO projects (id, name, slug, api_key, created_at) VALUES ('proj-2', 'Test2', 'test2', 'key2', 0)",
            [],
        ).unwrap();
    })
    .await
    .unwrap();

    // Insert a token that expires in 1 hour
    let plaintext = "bloop_tk_test_valid_token_abcdefghijklm";
    let hash = bloop::auth::bearer::hash_token(plaintext);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let expires_at = now + 3600;

    let h = hash.clone();
    conn.interact(move |conn| {
        conn.execute(
            "INSERT INTO bearer_tokens (id, name, token_hash, token_prefix, project_id, created_by, scopes, created_at, expires_at)
             VALUES ('tok-2', 'test', ?1, 'bloop_tk_test_', 'proj-2', 'user-1', '[\"errors:read\"]', ?2, ?3)",
            rusqlite::params![h, now, expires_at],
        ).unwrap();
    })
    .await
    .unwrap();

    let cache = bloop::auth::bearer::BearerTokenCache::new(pool);
    let result = cache.resolve(plaintext).await;
    assert!(result.is_some(), "valid token should be accepted");

    let auth = result.unwrap();
    assert_eq!(auth.project_id, "proj-2");
    assert_eq!(auth.scopes, vec!["errors:read"]);
    assert_eq!(auth.expires_at, Some(expires_at));
}

#[tokio::test]
async fn test_cached_token_rejected_after_expiry() {
    let pool = setup_pool().await;

    let conn = pool.get().await.unwrap();
    conn.interact(|conn| {
        conn.execute(
            "INSERT INTO projects (id, name, slug, api_key, created_at) VALUES ('proj-3', 'Test3', 'test3', 'key3', 0)",
            [],
        ).unwrap();
    })
    .await
    .unwrap();

    // Insert a token that expires in 1 second
    let plaintext = "bloop_tk_test_cache_expiry_token_123456";
    let hash = bloop::auth::bearer::hash_token(plaintext);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let expires_at = now + 1; // expires in 1 second

    let h = hash.clone();
    conn.interact(move |conn| {
        conn.execute(
            "INSERT INTO bearer_tokens (id, name, token_hash, token_prefix, project_id, created_by, scopes, created_at, expires_at)
             VALUES ('tok-3', 'test', ?1, 'bloop_tk_test_', 'proj-3', 'user-1', '[\"errors:read\"]', ?2, ?3)",
            rusqlite::params![h, now, expires_at],
        ).unwrap();
    })
    .await
    .unwrap();

    let cache = bloop::auth::bearer::BearerTokenCache::new(pool);

    // First resolve should succeed (populates cache)
    let result = cache.resolve(plaintext).await;
    assert!(result.is_some(), "should succeed before expiry");

    // Wait for token to expire
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Second resolve should fail (cache hit, but expires_at re-check fails)
    let result = cache.resolve(plaintext).await;
    assert!(
        result.is_none(),
        "should reject after expiry even with cached entry"
    );
}

// ═══════════════════════════════════════════════════════════════
// H-8: HMAC secret validation
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_hmac_secret_too_short_rejected() {
    let config = bloop::config::AppConfig {
        server: bloop::config::ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 5332,
        },
        database: bloop::config::DatabaseConfig {
            path: "test.db".into(),
            pool_size: 1,
        },
        ingest: bloop::config::IngestConfig {
            max_payload_bytes: 32768,
            max_stack_bytes: 8192,
            max_metadata_bytes: 4096,
            max_message_bytes: 2048,
            max_batch_size: 50,
            channel_capacity: 8192,
        },
        pipeline: bloop::config::PipelineConfig {
            flush_interval_secs: 2,
            flush_batch_size: 500,
            sample_reservoir_size: 5,
        },
        retention: bloop::config::RetentionConfig {
            raw_events_days: 7,
            prune_interval_secs: 3600,
        },
        auth: bloop::config::AuthConfig {
            hmac_secret: "short-secret".to_string(), // < 32 chars
            rp_id: "localhost".to_string(),
            rp_origin: "http://localhost:5332".to_string(),
            session_ttl_secs: 604800,
        },
        rate_limit: bloop::config::RateLimitConfig {
            per_second: 100,
            burst_size: 200,
            auth_per_second: 5,
            auth_burst_size: 10,
        },
        alerting: bloop::config::AlertingConfig { cooldown_secs: 900 },
        smtp: Default::default(),
        analytics: Default::default(),
        llm_tracing: Default::default(),
    };

    let result = config.validate();
    assert!(result.is_err(), "should reject short HMAC secret");
    assert!(result.unwrap_err().contains("at least 32 characters"));
}

#[test]
fn test_hmac_secret_valid_length_accepted() {
    let config = bloop::config::AppConfig {
        server: bloop::config::ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 5332,
        },
        database: bloop::config::DatabaseConfig {
            path: "test.db".into(),
            pool_size: 1,
        },
        ingest: bloop::config::IngestConfig {
            max_payload_bytes: 32768,
            max_stack_bytes: 8192,
            max_metadata_bytes: 4096,
            max_message_bytes: 2048,
            max_batch_size: 50,
            channel_capacity: 8192,
        },
        pipeline: bloop::config::PipelineConfig {
            flush_interval_secs: 2,
            flush_batch_size: 500,
            sample_reservoir_size: 5,
        },
        retention: bloop::config::RetentionConfig {
            raw_events_days: 7,
            prune_interval_secs: 3600,
        },
        auth: bloop::config::AuthConfig {
            hmac_secret: "a]b@c#d$e%f^g&h*i(j)k+l=m-n.o/p!".to_string(), // 33 chars
            rp_id: "localhost".to_string(),
            rp_origin: "http://localhost:5332".to_string(),
            session_ttl_secs: 604800,
        },
        rate_limit: bloop::config::RateLimitConfig {
            per_second: 100,
            burst_size: 200,
            auth_per_second: 5,
            auth_burst_size: 10,
        },
        alerting: bloop::config::AlertingConfig { cooldown_secs: 900 },
        smtp: Default::default(),
        analytics: Default::default(),
        llm_tracing: Default::default(),
    };

    let result = config.validate();
    assert!(
        result.is_ok(),
        "should accept valid HMAC secret: {:?}",
        result
    );
}

#[test]
fn test_hmac_empty_secret_rejected() {
    let config = bloop::config::AppConfig {
        server: bloop::config::ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 5332,
        },
        database: bloop::config::DatabaseConfig {
            path: "test.db".into(),
            pool_size: 1,
        },
        ingest: bloop::config::IngestConfig {
            max_payload_bytes: 32768,
            max_stack_bytes: 8192,
            max_metadata_bytes: 4096,
            max_message_bytes: 2048,
            max_batch_size: 50,
            channel_capacity: 8192,
        },
        pipeline: bloop::config::PipelineConfig {
            flush_interval_secs: 2,
            flush_batch_size: 500,
            sample_reservoir_size: 5,
        },
        retention: bloop::config::RetentionConfig {
            raw_events_days: 7,
            prune_interval_secs: 3600,
        },
        auth: bloop::config::AuthConfig {
            hmac_secret: "".to_string(),
            rp_id: "localhost".to_string(),
            rp_origin: "http://localhost:5332".to_string(),
            session_ttl_secs: 604800,
        },
        rate_limit: bloop::config::RateLimitConfig {
            per_second: 100,
            burst_size: 200,
            auth_per_second: 5,
            auth_burst_size: 10,
        },
        alerting: bloop::config::AlertingConfig { cooldown_secs: 900 },
        smtp: Default::default(),
        analytics: Default::default(),
        llm_tracing: Default::default(),
    };

    let result = config.validate();
    assert!(result.is_err(), "should reject empty HMAC secret");
}
