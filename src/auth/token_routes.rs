use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use deadpool_sqlite::Pool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::bearer::{generate_token, hash_token, BearerTokenCache};
use super::session::SessionUser;

pub struct TokenState {
    pub pool: Pool,
    pub cache: Arc<BearerTokenCache>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTokenRequest {
    pub name: String,
    pub project_id: String,
    pub scopes: Vec<String>,
    pub expires_in_days: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct CreateTokenResponse {
    pub id: String,
    pub name: String,
    pub token: String, // plaintext, shown once
    pub token_prefix: String,
    pub project_id: String,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub token_prefix: String,
    pub project_id: String,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ListTokensQuery {
    pub project_id: String,
}

const VALID_SCOPES: &[&str] = &[
    "errors:read",
    "errors:write",
    "sourcemaps:read",
    "sourcemaps:write",
    "alerts:read",
    "alerts:write",
];

/// POST /v1/tokens - Create a new bearer token (requires session auth).
pub async fn create_token(
    State(state): State<Arc<TokenState>>,
    axum::Extension(user): axum::Extension<SessionUser>,
    Json(input): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreateTokenResponse>), (StatusCode, Json<serde_json::Value>)> {
    // Validate name
    if input.name.trim().is_empty() || input.name.len() > 100 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "name must be 1-100 characters"})),
        ));
    }

    // Validate scopes
    if input.scopes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "at least one scope is required"})),
        ));
    }
    for scope in &input.scopes {
        if !VALID_SCOPES.contains(&scope.as_str()) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("invalid scope: {scope}")})),
            ));
        }
    }

    // Verify the project exists and user has access (must be admin or project creator)
    let project_id = input.project_id.clone();
    let conn = state.pool.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("pool error: {e}")})),
        )
    })?;

    let pid = project_id.clone();
    let check_uid = user.user_id.clone();
    let is_admin = user.is_admin;
    let created_by: Option<String> = conn
        .interact(move |conn| {
            conn.query_row(
                "SELECT created_by FROM projects WHERE id = ?1",
                rusqlite::params![pid],
                |row| row.get::<_, Option<String>>(0),
            )
        })
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("db error: {e}")})),
            )
        })?
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "project not found"})),
            )
        })?;

    if !is_admin && created_by.as_deref() != Some(&check_uid) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "you do not have access to this project"})),
        ));
    }

    // Generate token
    let plaintext = generate_token();
    let token_hash = hash_token(&plaintext);
    let token_prefix = plaintext[..16].to_string();
    let id = uuid::Uuid::new_v4().to_string();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let expires_at = input
        .expires_in_days
        .map(|days| now + (days as i64 * 86400));

    let scopes_json = serde_json::to_string(&input.scopes).unwrap();

    // Insert into DB
    let row_id = id.clone();
    let row_name = input.name.clone();
    let row_hash = token_hash;
    let row_prefix = token_prefix.clone();
    let row_project = project_id.clone();
    let row_user = user.user_id.clone();
    let row_scopes = scopes_json;
    let row_expires = expires_at;

    conn.interact(move |conn| {
        conn.execute(
            "INSERT INTO bearer_tokens (id, name, token_hash, token_prefix, project_id, created_by, scopes, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![row_id, row_name, row_hash, row_prefix, row_project, row_user, row_scopes, now, row_expires],
        )
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("db error: {e}")})),
        )
    })?
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("db error: {e}")})),
        )
    })?;

    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            id,
            name: input.name,
            token: plaintext,
            token_prefix,
            project_id,
            scopes: input.scopes,
            created_at: now,
            expires_at,
        }),
    ))
}

/// GET /v1/tokens - List tokens for a project (requires session auth).
pub async fn list_tokens(
    State(state): State<Arc<TokenState>>,
    axum::Extension(user): axum::Extension<SessionUser>,
    Query(params): Query<ListTokensQuery>,
) -> Result<Json<Vec<TokenInfo>>, (StatusCode, Json<serde_json::Value>)> {
    let conn = state.pool.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("pool error: {e}")})),
        )
    })?;

    // Verify user has access to this project (admin or creator)
    let check_pid = params.project_id.clone();
    let check_uid = user.user_id.clone();
    let is_admin = user.is_admin;
    let created_by: Option<String> = conn
        .interact(move |conn| {
            conn.query_row(
                "SELECT created_by FROM projects WHERE id = ?1",
                rusqlite::params![check_pid],
                |row| row.get::<_, Option<String>>(0),
            )
        })
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("db error: {e}")})),
            )
        })?
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "project not found"})),
            )
        })?;

    if !is_admin && created_by.as_deref() != Some(&check_uid) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "you do not have access to this project"})),
        ));
    }

    let project_id = params.project_id;
    let tokens = conn
        .interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, token_prefix, project_id, scopes, created_at, expires_at, last_used_at, revoked_at
                 FROM bearer_tokens
                 WHERE project_id = ?1
                 ORDER BY created_at DESC",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![project_id], |row| {
                    let scopes_json: String = row.get(4)?;
                    let scopes: Vec<String> =
                        serde_json::from_str(&scopes_json).unwrap_or_default();
                    Ok(TokenInfo {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        token_prefix: row.get(2)?,
                        project_id: row.get(3)?,
                        scopes,
                        created_at: row.get(5)?,
                        expires_at: row.get(6)?,
                        last_used_at: row.get(7)?,
                        revoked_at: row.get(8)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("db error: {e}")})),
            )
        })?
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("db error: {e}")})),
            )
        })?;

    Ok(Json(tokens))
}

/// DELETE /v1/tokens/:id - Revoke a token (requires session auth).
pub async fn revoke_token(
    State(state): State<Arc<TokenState>>,
    axum::Extension(user): axum::Extension<SessionUser>,
    Path(token_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let conn = state.pool.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("pool error: {e}")})),
        )
    })?;

    // Verify user owns this token or is admin
    let check_tid = token_id.clone();
    let check_uid = user.user_id.clone();
    let is_admin = user.is_admin;
    let token_owner: Option<(String, String)> = conn
        .interact(move |conn| {
            conn.query_row(
                "SELECT created_by, project_id FROM bearer_tokens WHERE id = ?1 AND revoked_at IS NULL",
                rusqlite::params![check_tid],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .ok()
        })
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("db error: {e}")})),
            )
        })?;

    match token_owner {
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "token not found or already revoked"})),
            ));
        }
        Some((created_by, project_id)) => {
            // Allow: admin, token creator, or project creator
            if !is_admin && created_by != check_uid {
                let proj_check_uid = check_uid.clone();
                let project_creator: Option<String> = conn
                    .interact(move |conn| {
                        conn.query_row(
                            "SELECT created_by FROM projects WHERE id = ?1",
                            rusqlite::params![project_id],
                            |row| row.get::<_, Option<String>>(0),
                        )
                        .ok()
                        .flatten()
                    })
                    .await
                    .map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({"error": format!("db error: {e}")})),
                        )
                    })?;

                if project_creator.as_deref() != Some(&proj_check_uid) {
                    return Err((
                        StatusCode::FORBIDDEN,
                        Json(
                            serde_json::json!({"error": "you do not have permission to revoke this token"}),
                        ),
                    ));
                }
            }
        }
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let tid = token_id.clone();
    let updated = conn
        .interact(move |conn| {
            conn.execute(
                "UPDATE bearer_tokens SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
                rusqlite::params![now, tid],
            )
        })
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("db error: {e}")})),
            )
        })?
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("db error: {e}")})),
            )
        })?;

    if updated == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "token not found or already revoked"})),
        ));
    }

    // Invalidate cache (simple approach; token-specific eviction would require reverse-lookup)
    state.cache.invalidate_all();

    Ok(Json(serde_json::json!({"revoked": token_id})))
}
