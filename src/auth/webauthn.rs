use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use webauthn_rs::prelude::*;

use super::session::{self, SessionUser};
use super::state::AuthState;

// ── Types ──

#[derive(Serialize)]
pub struct StatusResponse {
    pub setup_complete: bool,
}

#[derive(Deserialize)]
pub struct RegisterStartRequest {
    pub username: String,
}

#[derive(Serialize)]
pub struct ChallengeResponseWrapper {
    pub challenge_id: String,
    #[serde(flatten)]
    pub options: serde_json::Value,
}

#[derive(Deserialize)]
pub struct RegisterFinishRequest {
    pub challenge_id: String,
    pub credential: RegisterPublicKeyCredential,
}

#[derive(Deserialize)]
pub struct LoginFinishRequest {
    pub challenge_id: String,
    pub credential: PublicKeyCredential,
}

#[derive(Deserialize)]
pub struct InviteRegisterStartRequest {
    pub username: String,
    pub invite_token: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct InviteRegisterFinishRequest {
    pub challenge_id: String,
    pub credential: RegisterPublicKeyCredential,
    pub invite_token: String,
}

#[derive(Serialize)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
    pub is_admin: bool,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct InviteInfo {
    pub token: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub used: bool,
}

#[derive(Deserialize)]
pub struct AdminResetRequest {
    pub confirm: bool,
}

#[derive(Deserialize)]
pub struct UpdateUserRoleRequest {
    pub is_admin: bool,
}

// ── Helpers ──

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn new_challenge_id() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Default to Secure cookies unless the RP origin is explicitly localhost.
/// This ensures cookies are Secure even behind TLS-terminating reverse proxies
/// where the app sees http:// origins.
fn is_secure_deployment(state: &AuthState) -> bool {
    let origins = state.webauthn.get_allowed_origins();
    let is_localhost = origins.iter().all(|o| {
        matches!(
            o.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("[::1]")
        )
    });
    !is_localhost
}

fn session_cookie(token: &str, max_age: u64, secure: bool) -> String {
    let secure_flag = if secure { "; Secure" } else { "" };
    format!("bloop_session={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}{secure_flag}")
}

fn clear_cookie(secure: bool) -> String {
    let secure_flag = if secure { "; Secure" } else { "" };
    format!("bloop_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0{secure_flag}")
}

async fn user_count(state: &AuthState) -> Result<i64, Response> {
    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;
    conn.interact(|conn| {
        conn.query_row("SELECT COUNT(*) FROM webauthn_users", [], |row| {
            row.get::<_, i64>(0)
        })
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "interact error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?
    .map_err(|e| {
        tracing::error!(error = %e, "query error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })
}

#[allow(clippy::result_large_err)]
fn require_admin(session_user: &SessionUser) -> Result<(), Response> {
    if !session_user.is_admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "admin access required"})),
        )
            .into_response());
    }
    Ok(())
}

// ── Endpoints ──

/// GET /auth/status - Check if initial setup is complete.
pub async fn status(State(state): State<Arc<AuthState>>) -> Result<Json<StatusResponse>, Response> {
    let count = user_count(&state).await?;
    Ok(Json(StatusResponse {
        setup_complete: count > 0,
    }))
}

/// GET /auth/login - Serve the login/setup page.
pub async fn login_page() -> Html<&'static str> {
    Html(include_str!("../auth_login.html"))
}

/// POST /auth/register/start - Begin passkey registration (only if no users exist).
pub async fn register_start(
    State(state): State<Arc<AuthState>>,
    Json(body): Json<RegisterStartRequest>,
) -> Result<Response, Response> {
    // Guard: only allow registration if no users exist
    let count = user_count(&state).await?;
    if count > 0 {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "registration is closed"})),
        )
            .into_response());
    }

    let username = body.username.trim().to_string();
    if username.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "username is required"})),
        )
            .into_response());
    }

    // Create a temporary user UUID for the registration ceremony
    let user_id = uuid::Uuid::new_v4();

    let (ccr, reg_state) = state
        .webauthn
        .start_passkey_registration(user_id, &username, &username, None)
        .map_err(|e| {
            tracing::error!(error = %e, "webauthn registration start failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    // Serialize reg_state into challenge cache
    let challenge_id = new_challenge_id();
    let cache_value = serde_json::json!({
        "type": "registration",
        "user_id": user_id.to_string(),
        "username": username,
        "state": reg_state,
    });
    state.challenge_cache.insert(
        challenge_id.clone(),
        serde_json::to_string(&cache_value).unwrap(),
    );

    let response = ChallengeResponseWrapper {
        challenge_id,
        options: serde_json::to_value(ccr).unwrap(),
    };

    Ok(Json(response).into_response())
}

/// POST /auth/register/finish - Complete passkey registration.
pub async fn register_finish(
    State(state): State<Arc<AuthState>>,
    Json(body): Json<RegisterFinishRequest>,
) -> Result<Response, Response> {
    // Guard: only allow registration if no users exist
    let count = user_count(&state).await?;
    if count > 0 {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "registration is closed"})),
        )
            .into_response());
    }

    // Retrieve challenge state
    let cached = state
        .challenge_cache
        .remove(&body.challenge_id)
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "challenge expired or invalid"})),
            )
                .into_response()
        })?;

    let cached: serde_json::Value = serde_json::from_str(&cached)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;

    if cached["type"] != "registration" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "wrong challenge type"})),
        )
            .into_response());
    }

    let reg_state: PasskeyRegistration =
        serde_json::from_value(cached["state"].clone()).map_err(|e| {
            tracing::error!(error = %e, "failed to deserialize reg state");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    let user_id_str = cached["user_id"]
        .as_str()
        .ok_or_else(|| StatusCode::INTERNAL_SERVER_ERROR.into_response())?
        .to_string();
    let username = cached["username"]
        .as_str()
        .ok_or_else(|| StatusCode::INTERNAL_SERVER_ERROR.into_response())?
        .to_string();

    // Finish registration
    let passkey = state
        .webauthn
        .finish_passkey_registration(&body.credential, &reg_state)
        .map_err(|e| {
            tracing::error!(error = %e, "webauthn registration finish failed");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "registration failed"})),
            )
                .into_response()
        })?;

    // Store user + credential in DB
    let now = now_epoch();
    let cred_json = serde_json::to_string(&passkey).map_err(|e| {
        tracing::error!(error = %e, "failed to serialize passkey");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let uid = user_id_str.clone();
    let uname = username.clone();
    let cjson = cred_json.clone();
    conn.interact(move |conn| {
        conn.execute(
            "INSERT INTO webauthn_users (id, username, display_name, created_at, is_admin) VALUES (?1, ?2, ?3, ?4, 1)",
            rusqlite::params![uid, uname, uname, now],
        )?;
        conn.execute(
            "INSERT INTO webauthn_credentials (user_id, credential_json, name, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![uid, cjson, "default", now],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "interact error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?
    .map_err(|e| {
        tracing::error!(error = %e, "db error during registration");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    tracing::info!(user = %username, "passkey registered");

    // Create session
    let token = session::create_session(&state.pool, &user_id_str, state.session_ttl_secs)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create session");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    let secure = is_secure_deployment(&state);

    Ok((
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            session_cookie(&token, state.session_ttl_secs, secure),
        )],
        Json(serde_json::json!({"status": "ok"})),
    )
        .into_response())
}

/// POST /auth/login/start - Begin passkey authentication.
pub async fn login_start(State(state): State<Arc<AuthState>>) -> Result<Response, Response> {
    // Load all credentials from DB
    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let creds: Vec<Passkey> = conn
        .interact(|conn| {
            let mut stmt = conn
                .prepare("SELECT credential_json FROM webauthn_credentials")
                .unwrap();
            let rows = stmt
                .query_map([], |row| {
                    let json_str: String = row.get(0)?;
                    Ok(json_str)
                })
                .unwrap();
            let mut passkeys = Vec::new();
            for json_str in rows.flatten() {
                if let Ok(pk) = serde_json::from_str::<Passkey>(&json_str) {
                    passkeys.push(pk);
                }
            }
            passkeys
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "interact error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    if creds.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "no credentials registered"})),
        )
            .into_response());
    }

    let (rcr, auth_state) = state
        .webauthn
        .start_passkey_authentication(&creds)
        .map_err(|e| {
            tracing::error!(error = %e, "webauthn auth start failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    let challenge_id = new_challenge_id();
    let cache_value = serde_json::json!({
        "type": "authentication",
        "state": auth_state,
    });
    state.challenge_cache.insert(
        challenge_id.clone(),
        serde_json::to_string(&cache_value).unwrap(),
    );

    let response = ChallengeResponseWrapper {
        challenge_id,
        options: serde_json::to_value(rcr).unwrap(),
    };

    Ok(Json(response).into_response())
}

/// POST /auth/login/finish - Complete passkey authentication.
pub async fn login_finish(
    State(state): State<Arc<AuthState>>,
    Json(body): Json<LoginFinishRequest>,
) -> Result<Response, Response> {
    let cached = state
        .challenge_cache
        .remove(&body.challenge_id)
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "challenge expired or invalid"})),
            )
                .into_response()
        })?;

    let cached: serde_json::Value = serde_json::from_str(&cached)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;

    if cached["type"] != "authentication" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "wrong challenge type"})),
        )
            .into_response());
    }

    let auth_state: PasskeyAuthentication = serde_json::from_value(cached["state"].clone())
        .map_err(|e| {
            tracing::error!(error = %e, "failed to deserialize auth state");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    let auth_result = state
        .webauthn
        .finish_passkey_authentication(&body.credential, &auth_state)
        .map_err(|e| {
            tracing::error!(error = %e, "webauthn auth finish failed");
            (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "authentication failed"})),
            )
                .into_response()
        })?;

    // Update credential counter in DB if needed
    let now = now_epoch();
    let cred_id_bytes = auth_result.cred_id().to_owned();
    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    // Find the user_id for the authenticated credential and update counter
    let user_id: String = conn
        .interact(move |conn| {
            // Find credential that matches and update last_used_at + counter
            let mut stmt = conn
                .prepare("SELECT id, user_id, credential_json FROM webauthn_credentials")
                .unwrap();
            let rows: Vec<(i64, String, String)> = stmt
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();

            for (id, user_id, json_str) in &rows {
                if let Ok(pk) = serde_json::from_str::<Passkey>(json_str) {
                    if pk.cred_id() == &cred_id_bytes {
                        // Update the credential with new counter if needed
                        if auth_result.needs_update() {
                            let mut updated_pk = pk;
                            updated_pk.update_credential(&auth_result);
                            if let Ok(new_json) = serde_json::to_string(&updated_pk) {
                                let _ = conn.execute(
                                    "UPDATE webauthn_credentials SET credential_json = ?1, last_used_at = ?2 WHERE id = ?3",
                                    rusqlite::params![new_json, now, id],
                                );
                            }
                        } else {
                            let _ = conn.execute(
                                "UPDATE webauthn_credentials SET last_used_at = ?1 WHERE id = ?2",
                                rusqlite::params![now, id],
                            );
                        }
                        return Ok(user_id.clone());
                    }
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows)
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "interact error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?
        .map_err(|e| {
            tracing::error!(error = %e, "credential lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    // Create session
    let token = session::create_session(&state.pool, &user_id, state.session_ttl_secs)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create session");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    let secure = is_secure_deployment(&state);

    tracing::info!(user_id = %user_id, "passkey login successful");

    Ok((
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            session_cookie(&token, state.session_ttl_secs, secure),
        )],
        Json(serde_json::json!({"status": "ok"})),
    )
        .into_response())
}

/// POST /auth/logout - Destroy session.
pub async fn logout(
    State(state): State<Arc<AuthState>>,
    request: axum::extract::Request,
) -> Response {
    // Extract session cookie
    if let Some(cookie_header) = request.headers().get(header::COOKIE) {
        if let Ok(cookie_str) = cookie_header.to_str() {
            for part in cookie_str.split(';') {
                let trimmed = part.trim();
                if let Some(value) = trimmed.strip_prefix("bloop_session=") {
                    let _ = session::delete_session(&state.pool, value).await;
                }
            }
        }
    }

    let secure = is_secure_deployment(&state);

    (
        StatusCode::OK,
        [(header::SET_COOKIE, clear_cookie(secure))],
        Json(serde_json::json!({"status": "ok"})),
    )
        .into_response()
}

// ── Invite Registration Endpoints ──

/// POST /auth/invite/register/start - Begin registration with invite token.
pub async fn invite_register_start(
    State(state): State<Arc<AuthState>>,
    Json(body): Json<InviteRegisterStartRequest>,
) -> Result<Response, Response> {
    let username = body.username.trim().to_string();
    if username.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "username is required"})),
        )
            .into_response());
    }

    // Validate invite token
    let invite_token = body.invite_token.clone();
    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let now = now_epoch();
    let tk = invite_token.clone();
    let valid: bool = conn
        .interact(move |conn| {
            conn.query_row(
                "SELECT COUNT(*) > 0 FROM invites WHERE token = ?1 AND used_by IS NULL AND expires_at > ?2",
                rusqlite::params![tk, now],
                |row| row.get(0),
            )
            .unwrap_or(false)
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "interact error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    if !valid {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invite token is invalid or expired"})),
        )
            .into_response());
    }

    let user_id = uuid::Uuid::new_v4();

    let (ccr, reg_state) = state
        .webauthn
        .start_passkey_registration(user_id, &username, &username, None)
        .map_err(|e| {
            tracing::error!(error = %e, "webauthn registration start failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    let challenge_id = new_challenge_id();
    let cache_value = serde_json::json!({
        "type": "invite_registration",
        "user_id": user_id.to_string(),
        "username": username,
        "invite_token": invite_token,
        "state": reg_state,
    });
    state.challenge_cache.insert(
        challenge_id.clone(),
        serde_json::to_string(&cache_value).unwrap(),
    );

    let response = ChallengeResponseWrapper {
        challenge_id,
        options: serde_json::to_value(ccr).unwrap(),
    };

    Ok(Json(response).into_response())
}

/// POST /auth/invite/register/finish - Complete invite registration + create session.
pub async fn invite_register_finish(
    State(state): State<Arc<AuthState>>,
    Json(body): Json<InviteRegisterFinishRequest>,
) -> Result<Response, Response> {
    // Retrieve challenge state
    let cached = state
        .challenge_cache
        .remove(&body.challenge_id)
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "challenge expired or invalid"})),
            )
                .into_response()
        })?;

    let cached: serde_json::Value = serde_json::from_str(&cached)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;

    if cached["type"] != "invite_registration" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "wrong challenge type"})),
        )
            .into_response());
    }

    let reg_state: PasskeyRegistration =
        serde_json::from_value(cached["state"].clone()).map_err(|e| {
            tracing::error!(error = %e, "failed to deserialize reg state");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    let user_id_str = cached["user_id"]
        .as_str()
        .ok_or_else(|| StatusCode::INTERNAL_SERVER_ERROR.into_response())?
        .to_string();
    let username = cached["username"]
        .as_str()
        .ok_or_else(|| StatusCode::INTERNAL_SERVER_ERROR.into_response())?
        .to_string();
    let invite_token = cached["invite_token"]
        .as_str()
        .ok_or_else(|| StatusCode::INTERNAL_SERVER_ERROR.into_response())?
        .to_string();

    // Re-validate invite token
    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let now = now_epoch();
    let tk = invite_token.clone();
    let valid: bool = conn
        .interact(move |conn| {
            conn.query_row(
                "SELECT COUNT(*) > 0 FROM invites WHERE token = ?1 AND used_by IS NULL AND expires_at > ?2",
                rusqlite::params![tk, now],
                |row| row.get(0),
            )
            .unwrap_or(false)
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "interact error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    if !valid {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invite token is invalid or expired"})),
        )
            .into_response());
    }

    // Finish registration
    let passkey = state
        .webauthn
        .finish_passkey_registration(&body.credential, &reg_state)
        .map_err(|e| {
            tracing::error!(error = %e, "webauthn registration finish failed");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "registration failed"})),
            )
                .into_response()
        })?;

    // Store user + credential + mark invite used
    let cred_json = serde_json::to_string(&passkey).map_err(|e| {
        tracing::error!(error = %e, "failed to serialize passkey");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let uid = user_id_str.clone();
    let uname = username.clone();
    let cjson = cred_json.clone();
    let tk2 = invite_token.clone();
    let uid2 = user_id_str.clone();
    conn.interact(move |conn| {
        conn.execute(
            "INSERT INTO webauthn_users (id, username, display_name, created_at, is_admin) VALUES (?1, ?2, ?3, ?4, 0)",
            rusqlite::params![uid, uname, uname, now],
        )?;
        conn.execute(
            "INSERT INTO webauthn_credentials (user_id, credential_json, name, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![uid, cjson, "default", now],
        )?;
        let updated = conn.execute(
            "UPDATE invites SET used_by = ?1, used_at = ?2 WHERE token = ?3 AND used_by IS NULL",
            rusqlite::params![uid2, now, tk2],
        )?;
        if updated == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "interact error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?
    .map_err(|e| {
        tracing::error!(error = %e, "db error during invite registration");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    tracing::info!(user = %username, "invite passkey registered");

    // Create session
    let token = session::create_session(&state.pool, &user_id_str, state.session_ttl_secs)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create session");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    let secure = is_secure_deployment(&state);

    Ok((
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            session_cookie(&token, state.session_ttl_secs, secure),
        )],
        Json(serde_json::json!({"status": "ok"})),
    )
        .into_response())
}

// ── Admin Endpoints ──

/// GET /v1/admin/users - List all users.
pub async fn list_users(
    State(state): State<Arc<AuthState>>,
    request: axum::extract::Request,
) -> Result<Response, Response> {
    let session_user = request
        .extensions()
        .get::<SessionUser>()
        .cloned()
        .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
    require_admin(&session_user)?;

    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let users: Vec<UserInfo> = conn
        .interact(|conn| {
            let mut stmt = conn
                .prepare("SELECT id, username, is_admin, created_at FROM webauthn_users ORDER BY created_at ASC")
                .unwrap();
            stmt.query_map([], |row| {
                Ok(UserInfo {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    is_admin: row.get::<_, i64>(2)? != 0,
                    created_at: row.get(3)?,
                })
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "interact error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    Ok(Json(users).into_response())
}

/// DELETE /v1/admin/users/{user_id} - Remove a user (not self).
pub async fn delete_user(
    State(state): State<Arc<AuthState>>,
    Path(target_user_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Response, Response> {
    let session_user = request
        .extensions()
        .get::<SessionUser>()
        .cloned()
        .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
    require_admin(&session_user)?;

    if target_user_id == session_user.user_id {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "cannot delete yourself"})),
        )
            .into_response());
    }

    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let uid = target_user_id.clone();
    conn.interact(move |conn| {
        conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1",
            rusqlite::params![uid],
        )?;
        conn.execute(
            "DELETE FROM webauthn_credentials WHERE user_id = ?1",
            rusqlite::params![uid],
        )?;
        conn.execute(
            "DELETE FROM webauthn_users WHERE id = ?1",
            rusqlite::params![uid],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "interact error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?
    .map_err(|e| {
        tracing::error!(error = %e, "db error deleting user");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    tracing::info!(user_id = %target_user_id, "user deleted by admin");
    Ok(Json(serde_json::json!({"status": "ok"})).into_response())
}

/// GET /v1/admin/invites - List invites.
pub async fn list_invites(
    State(state): State<Arc<AuthState>>,
    request: axum::extract::Request,
) -> Result<Response, Response> {
    let session_user = request
        .extensions()
        .get::<SessionUser>()
        .cloned()
        .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
    require_admin(&session_user)?;

    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let invites: Vec<InviteInfo> = conn
        .interact(|conn| {
            let mut stmt = conn
                .prepare("SELECT token, created_at, expires_at, used_by IS NOT NULL FROM invites ORDER BY created_at DESC")
                .unwrap();
            stmt.query_map([], |row| {
                Ok(InviteInfo {
                    token: row.get(0)?,
                    created_at: row.get(1)?,
                    expires_at: row.get(2)?,
                    used: row.get(3)?,
                })
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "interact error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    Ok(Json(invites).into_response())
}

/// POST /v1/admin/invites - Create a 7-day invite token.
pub async fn create_invite(
    State(state): State<Arc<AuthState>>,
    request: axum::extract::Request,
) -> Result<Response, Response> {
    let session_user = request
        .extensions()
        .get::<SessionUser>()
        .cloned()
        .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
    require_admin(&session_user)?;

    let token = new_challenge_id(); // reuse random token generator
    let now = now_epoch();
    let expires_at = now + 7 * 24 * 3600; // 7 days

    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let tk = token.clone();
    let uid = session_user.user_id.clone();
    conn.interact(move |conn| {
        conn.execute(
            "INSERT INTO invites (token, created_by, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![tk, uid, now, expires_at],
        )
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "interact error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?
    .map_err(|e| {
        tracing::error!(error = %e, "db error creating invite");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    tracing::info!(admin = %session_user.user_id, "invite created");
    Ok(Json(serde_json::json!({"token": token, "expires_at": expires_at})).into_response())
}

/// PUT /v1/admin/users/{user_id}/role - Toggle admin role.
pub async fn update_user_role(
    State(state): State<Arc<AuthState>>,
    Path(target_user_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Response, Response> {
    let session_user = request
        .extensions()
        .get::<SessionUser>()
        .cloned()
        .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
    require_admin(&session_user)?;

    if target_user_id == session_user.user_id {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "cannot change your own role"})),
        )
            .into_response());
    }

    let body_bytes = axum::body::to_bytes(request.into_body(), 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST.into_response())?;
    let input: UpdateUserRoleRequest = serde_json::from_slice(&body_bytes).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "request body must contain {\"is_admin\": true/false}"})),
        )
            .into_response()
    })?;

    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    let uid = target_user_id.clone();
    let is_admin_val = input.is_admin as i64;
    let updated = conn
        .interact(move |conn| {
            conn.execute(
                "UPDATE webauthn_users SET is_admin = ?1 WHERE id = ?2",
                rusqlite::params![is_admin_val, uid],
            )
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "interact error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?
        .map_err(|e| {
            tracing::error!(error = %e, "db error updating role");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

    if updated == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "user not found"})),
        )
            .into_response());
    }

    tracing::info!(
        user_id = %target_user_id,
        is_admin = input.is_admin,
        admin = %session_user.user_id,
        "user role updated"
    );

    Ok(Json(serde_json::json!({
        "status": "ok",
        "user_id": target_user_id,
        "is_admin": input.is_admin,
    }))
    .into_response())
}

/// POST /v1/admin/reset - Delete ALL auth data, clear cookie.
/// Requires `{"confirm": true}` in the request body.
pub async fn admin_reset(
    State(state): State<Arc<AuthState>>,
    request: axum::extract::Request,
) -> Result<Response, Response> {
    let session_user = request
        .extensions()
        .get::<SessionUser>()
        .cloned()
        .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
    require_admin(&session_user)?;

    // Extract and validate confirmation from body
    let body = axum::body::to_bytes(request.into_body(), 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST.into_response())?;
    let confirm: AdminResetRequest = serde_json::from_slice(&body).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "request body must contain {\"confirm\": true}"})),
        )
            .into_response()
    })?;
    if !confirm.confirm {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "confirm must be true to reset all auth data"})),
        )
            .into_response());
    }

    let conn = state.pool.get().await.map_err(|e| {
        tracing::error!(error = %e, "pool error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    conn.interact(|conn| {
        conn.execute_batch(
            "DELETE FROM invites;
             DELETE FROM sessions;
             DELETE FROM webauthn_credentials;
             DELETE FROM webauthn_users;",
        )
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "interact error");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?
    .map_err(|e| {
        tracing::error!(error = %e, "db error during reset");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;

    tracing::warn!(admin = %session_user.user_id, "all auth data reset");

    let secure = is_secure_deployment(&state);

    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, clear_cookie(secure))],
        Json(serde_json::json!({"status": "ok"})),
    )
        .into_response())
}
