use axum::{
    body::Body,
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use deadpool_sqlite::Pool;
use moka::sync::Cache;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::session::SessionUser;

/// Injected into request extensions after successful bearer token validation.
#[derive(Clone, Debug)]
pub struct TokenAuth {
    pub token_id: String,
    pub project_id: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<i64>,
}

/// Cached bearer token lookup: SHA-256 hash → TokenAuth.
#[derive(Clone)]
pub struct BearerTokenCache {
    cache: Cache<String, TokenAuth>,
    pool: Pool,
}

impl BearerTokenCache {
    pub fn new(pool: Pool) -> Self {
        let cache = Cache::builder()
            .max_capacity(1000)
            .time_to_live(Duration::from_secs(300))
            .build();
        Self { cache, pool }
    }

    /// Resolve a plaintext token to TokenAuth. Hashes the token, checks cache, then DB.
    pub async fn resolve(&self, plaintext: &str) -> Option<TokenAuth> {
        let hash = hash_token(plaintext);

        // Check cache (re-validate expiry on every hit)
        if let Some(auth) = self.cache.get(&hash) {
            if let Some(exp) = auth.expires_at {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;
                if exp < now {
                    self.cache.invalidate(&hash);
                    return None;
                }
            }
            return Some(auth);
        }

        // Query DB
        let h = hash.clone();
        let conn = self.pool.get().await.ok()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let result = conn
            .interact(move |conn| {
                conn.query_row(
                    "SELECT id, project_id, scopes, expires_at
                     FROM bearer_tokens
                     WHERE token_hash = ?1 AND revoked_at IS NULL",
                    rusqlite::params![h],
                    |row| {
                        let expires_at: Option<i64> = row.get(3)?;
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            expires_at,
                        ))
                    },
                )
            })
            .await
            .ok()?;

        let (id, project_id, scopes_json, expires_at) = result.ok()?;

        // Check expiry
        if let Some(exp) = expires_at {
            if exp < now {
                return None;
            }
        }

        let scopes: Vec<String> = serde_json::from_str(&scopes_json).ok()?;
        let auth = TokenAuth {
            token_id: id,
            project_id,
            scopes,
            expires_at,
        };

        self.cache.insert(hash, auth.clone());
        Some(auth)
    }

    /// Invalidate all cached entries (used after token revocation).
    pub fn invalidate_all(&self) {
        self.cache.invalidate_all();
    }

    /// Update last_used_at in the background (fire-and-forget).
    pub fn touch_last_used(&self, token_id: String) {
        let pool = self.pool.clone();
        tokio::spawn(async move {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            if let Ok(conn) = pool.get().await {
                let _ = conn
                    .interact(move |conn| {
                        conn.execute(
                            "UPDATE bearer_tokens SET last_used_at = ?1 WHERE id = ?2",
                            rusqlite::params![now, token_id],
                        )
                    })
                    .await;
            }
        });
    }
}

/// SHA-256 hash of a plaintext token, returned as hex.
pub fn hash_token(plaintext: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plaintext.as_bytes());
    hex::encode(hasher.finalize())
}

/// Extract Bearer token from Authorization header.
fn extract_bearer(req: &Request<Body>) -> Option<String> {
    let value = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

/// Middleware: tries Bearer token first, falls back to session cookie.
/// On success, injects either `TokenAuth` or `SessionUser` into extensions.
pub async fn require_bearer_or_session(
    request: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    // Try bearer token first
    if let Some(plaintext) = extract_bearer(&request) {
        let cache = request
            .extensions()
            .get::<Arc<BearerTokenCache>>()
            .cloned()
            .ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "bearer cache not configured",
                )
                    .into_response()
            })?;

        let auth = cache.resolve(&plaintext).await.ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({"error": "invalid or expired token"})),
            )
                .into_response()
        })?;

        cache.touch_last_used(auth.token_id.clone());

        let mut request = request;
        request.extensions_mut().insert(auth);
        return Ok(next.run(request).await);
    }

    // Fall back to session cookie (reuse existing session validation logic)
    let pool = request
        .extensions()
        .get::<Arc<Pool>>()
        .cloned()
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "session pool not configured",
            )
                .into_response()
        })?;

    let cookie_header = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let session_token = cookie_header
        .split(';')
        .find_map(|part| {
            let trimmed = part.trim();
            trimmed
                .strip_prefix("bloop_session=")
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string())
        })
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({"error": "authentication required"})),
            )
                .into_response()
        })?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let token = hash_token(&session_token);

    let conn = pool
        .get()
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "pool error").into_response())?;

    let session_user = conn
        .interact(move |conn| {
            conn.query_row(
                "SELECT s.user_id, COALESCE(u.is_admin, 0) FROM sessions s JOIN webauthn_users u ON s.user_id = u.id WHERE s.token = ?1 AND s.expires_at > ?2",
                rusqlite::params![token, now],
                |row| {
                    Ok(SessionUser {
                        user_id: row.get(0)?,
                        is_admin: row.get::<_, i64>(1)? != 0,
                    })
                },
            )
            .ok()
        })
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "session query error",
            )
                .into_response()
        })?
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({"error": "invalid or expired session"})),
            )
                .into_response()
        })?;

    let mut request = request;
    request.extensions_mut().insert(session_user);
    Ok(next.run(request).await)
}

/// Generate a new plaintext bearer token with the `bloop_tk_` prefix.
pub fn generate_token() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use rand::RngCore;

    let mut bytes = [0u8; 32]; // 256-bit entropy
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("bloop_tk_{}", URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_token_deterministic() {
        let hash1 = hash_token("test-token-abc");
        let hash2 = hash_token("test-token-abc");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_token_different_inputs() {
        let hash1 = hash_token("token-a");
        let hash2 = hash_token("token-b");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_token_is_hex() {
        let hash = hash_token("anything");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hash.len(), 64); // SHA-256 = 32 bytes = 64 hex chars
    }

    #[test]
    fn test_generate_token_has_prefix() {
        let token = generate_token();
        assert!(
            token.starts_with("bloop_tk_"),
            "token should start with bloop_tk_ prefix"
        );
    }

    #[test]
    fn test_generate_token_length() {
        let token = generate_token();
        // "bloop_tk_" (9 chars) + base64url(32 bytes) = 9 + 43 = 52 chars
        assert!(
            token.len() >= 50,
            "token should be at least 50 chars, got {}",
            token.len()
        );
    }

    #[test]
    fn test_generate_token_uniqueness() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2, "two generated tokens should differ");
    }
}
