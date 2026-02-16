use axum::{
    body::Body,
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use deadpool_sqlite::Pool;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::bearer::hash_token;

/// Injected into request extensions after successful session validation.
#[derive(Clone, Debug)]
pub struct SessionUser {
    pub user_id: String,
    pub is_admin: bool,
}

/// Extract the `bloop_session` cookie value from the Cookie header.
fn extract_session_cookie(req: &Request<Body>) -> Option<String> {
    let cookie_header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let trimmed = part.trim();
        if let Some(value) = trimmed.strip_prefix("bloop_session=") {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Validate a session token against the database. Returns SessionUser if valid.
async fn validate_session(pool: &Pool, token: &str) -> Option<SessionUser> {
    let conn = pool.get().await.ok()?;
    let token = hash_token(token);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    conn.interact(move |conn| {
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
    .ok()?
}

/// Session middleware for API routes — returns 401 JSON on failure.
pub async fn require_session_api(request: Request<Body>, next: Next) -> Result<Response, Response> {
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

    let token = extract_session_cookie(&request).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "authentication required"})),
        )
            .into_response()
    })?;

    let session_user = validate_session(&pool, &token).await.ok_or_else(|| {
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

/// Session middleware for the dashboard route — redirects to /auth/login on failure.
pub async fn require_session_browser(
    request: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    let pool = request
        .extensions()
        .get::<Arc<Pool>>()
        .cloned()
        .ok_or_else(|| Redirect::to("/auth/login").into_response())?;

    let token = extract_session_cookie(&request)
        .ok_or_else(|| Redirect::to("/auth/login").into_response())?;

    let session_user = validate_session(&pool, &token)
        .await
        .ok_or_else(|| Redirect::to("/auth/login").into_response())?;

    let mut request = request;
    request.extensions_mut().insert(session_user);
    Ok(next.run(request).await)
}

/// Create a session in the database and return the token.
pub async fn create_session(
    pool: &Pool,
    user_id: &str,
    ttl_secs: u64,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use rand::RngCore;

    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let expires_at = now + ttl_secs as i64;

    let conn = pool.get().await?;
    let token_hash = hash_token(&token);
    let uid = user_id.to_string();
    conn.interact(move |conn| {
        conn.execute(
            "INSERT INTO sessions (token, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![token_hash, uid, now, expires_at],
        )
    })
    .await
    .map_err(|e| format!("interact error: {e}"))?
    .map_err(|e| format!("db error: {e}"))?;

    Ok(token)
}

/// Delete a session from the database.
pub async fn delete_session(
    pool: &Pool,
    token: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let conn = pool.get().await?;
    let t = hash_token(token);
    conn.interact(move |conn| {
        conn.execute(
            "DELETE FROM sessions WHERE token = ?1",
            rusqlite::params![t],
        )
    })
    .await
    .map_err(|e| format!("interact error: {e}"))?
    .map_err(|e| format!("db error: {e}"))?;
    Ok(())
}

/// Periodically clean up expired sessions.
pub async fn session_cleanup_loop(pool: Pool) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        if let Ok(conn) = pool.get().await {
            let _ = conn
                .interact(move |conn| {
                    conn.execute(
                        "DELETE FROM sessions WHERE expires_at < ?1",
                        rusqlite::params![now],
                    )
                })
                .await;
        }
    }
}
