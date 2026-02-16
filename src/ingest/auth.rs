use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use deadpool_sqlite::Pool;
use hmac::{Hmac, Mac};
use moka::sync::Cache;
use sha2::Sha256;
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Cached project auth info: api_key → ProjectAuth.
#[derive(Clone)]
pub struct ProjectKeyCache {
    cache: Cache<String, ProjectAuth>,
    pool: Pool,
    /// Legacy HMAC secret for backward compat (used when no X-Project-Key header).
    legacy_secret: Option<String>,
}

/// Auth info resolved from an API key.
#[derive(Clone, Debug)]
pub struct ProjectAuth {
    pub project_id: String,
    pub api_key: String,
}

impl ProjectKeyCache {
    pub fn new(pool: Pool, legacy_secret: Option<String>) -> Self {
        let cache = Cache::builder()
            .max_capacity(1000)
            .time_to_live(Duration::from_secs(300))
            .build();
        Self {
            cache,
            pool,
            legacy_secret,
        }
    }

    /// Look up a project by API key, using cache.
    pub async fn resolve(&self, api_key: &str) -> Option<ProjectAuth> {
        // Check cache first
        if let Some(auth) = self.cache.get(api_key) {
            return Some(auth);
        }

        // Query DB
        let key = api_key.to_string();
        let conn = self.pool.get().await.ok()?;
        let result = conn
            .interact(move |conn| {
                conn.query_row(
                    "SELECT id, api_key FROM projects WHERE api_key = ?1",
                    rusqlite::params![key],
                    |row| {
                        Ok(ProjectAuth {
                            project_id: row.get(0)?,
                            api_key: row.get(1)?,
                        })
                    },
                )
            })
            .await
            .ok()?;

        match result {
            Ok(auth) => {
                self.cache.insert(api_key.to_string(), auth.clone());
                Some(auth)
            }
            Err(_) => None,
        }
    }
}

/// Legacy HMAC secret (kept for backward compat in tests/migration).
#[derive(Clone)]
pub struct HmacSecret(pub String);

/// Maximum body size for HMAC verification (must match ingest body limit).
#[derive(Clone)]
pub struct HmacBodyLimit(pub usize);

/// HMAC auth middleware with project key support.
///
/// Authentication modes:
/// 1. `X-Project-Key` header present: look up project by API key, use API key as HMAC secret
/// 2. No `X-Project-Key`: fall back to legacy `hmac_secret` → default project
pub async fn hmac_auth(request: Request<Body>, next: Next) -> Result<Response, impl IntoResponse> {
    // Try to get ProjectKeyCache (new path)
    let key_cache = request.extensions().get::<Arc<ProjectKeyCache>>().cloned();

    // Get legacy secret as fallback
    let legacy_secret = request.extensions().get::<HmacSecret>().cloned();

    let project_key_header = request
        .headers()
        .get("x-project-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let signature_header = request
        .headers()
        .get("x-signature")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let Some(signature_hex) = signature_header else {
        return Err((StatusCode::UNAUTHORIZED, "missing X-Signature header"));
    };

    let signature_bytes = hex::decode(&signature_hex)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid signature encoding"))?;

    // Read body to verify HMAC (limit matches configured max_payload_bytes)
    let body_limit = request
        .extensions()
        .get::<HmacBodyLimit>()
        .map(|l| l.0)
        .unwrap_or(64 * 1024);
    let (parts, body) = request.into_parts();
    let body_bytes = axum::body::to_bytes(body, body_limit)
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "failed to read body"))?;

    // Determine the HMAC secret and project auth
    let (hmac_secret_str, project_auth) = if let Some(ref api_key) = project_key_header {
        // New path: look up project by API key
        if let Some(ref cache) = key_cache {
            // We need to block here since we're in a sync-ish context
            // Use the api_key itself as the HMAC secret
            let auth = cache
                .resolve(api_key)
                .await
                .ok_or((StatusCode::UNAUTHORIZED, "invalid project key"))?;
            (auth.api_key.clone(), Some(auth))
        } else {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "auth not configured"));
        }
    } else if let Some(ref secret) = legacy_secret {
        // Legacy path: use hmac_secret, default project
        (
            secret.0.clone(),
            Some(ProjectAuth {
                project_id: "default".to_string(),
                api_key: secret.0.clone(),
            }),
        )
    } else if let Some(ref cache) = key_cache {
        // Try legacy secret from cache
        if let Some(ref ls) = cache.legacy_secret {
            (
                ls.clone(),
                Some(ProjectAuth {
                    project_id: "default".to_string(),
                    api_key: ls.clone(),
                }),
            )
        } else {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "auth not configured"));
        }
    } else {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "auth not configured"));
    };

    let mut mac = HmacSha256::new_from_slice(hmac_secret_str.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(&body_bytes);
    let expected = mac.finalize().into_bytes();

    if expected.as_slice().ct_eq(&signature_bytes).into() {
        // Reconstruct request with the body bytes + project auth extension
        let mut request = Request::from_parts(parts, Body::from(body_bytes));
        if let Some(auth) = project_auth {
            request.extensions_mut().insert(auth);
        }
        Ok(next.run(request).await)
    } else {
        Err((StatusCode::UNAUTHORIZED, "invalid signature"))
    }
}
