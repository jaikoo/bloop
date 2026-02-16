use crate::auth::session::SessionUser;
use crate::error::{AppError, AppResult};
use axum::extract::{Path, State};
use axum::Json;
use deadpool_sqlite::Pool;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub api_key: String,
    pub created_at: i64,
    pub created_by: Option<String>,
}

/// Project without sensitive fields (api_key) for list/get responses.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub created_at: i64,
    pub created_by: Option<String>,
}

impl From<Project> for ProjectSummary {
    fn from(p: Project) -> Self {
        Self {
            id: p.id,
            name: p.name,
            slug: p.slug,
            created_at: p.created_at,
            created_by: p.created_by,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateProject {
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProject {
    pub name: Option<String>,
}

/// GET /v1/projects - List all projects (without API keys).
pub async fn list_projects(State(pool): State<Arc<Pool>>) -> AppResult<Json<Vec<ProjectSummary>>> {
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let projects = conn
        .interact(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, slug, created_at, created_by
                 FROM projects ORDER BY created_at",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(ProjectSummary {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        slug: row.get(2)?,
                        created_at: row.get(3)?,
                        created_by: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(Json(projects))
}

/// POST /v1/projects - Create a new project.
pub async fn create_project(
    State(pool): State<Arc<Pool>>,
    session_user: Option<axum::Extension<SessionUser>>,
    Json(input): Json<CreateProject>,
) -> AppResult<Json<Project>> {
    if input.name.is_empty() {
        return Err(AppError::Validation("name is required".to_string()));
    }
    if input.slug.is_empty() {
        return Err(AppError::Validation("slug is required".to_string()));
    }
    // Validate slug format (lowercase, alphanumeric, hyphens)
    if !input
        .slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(AppError::Validation(
            "slug must be lowercase alphanumeric with hyphens".to_string(),
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let api_key = generate_api_key();
    let now = chrono::Utc::now().timestamp();

    let project = Project {
        id: id.clone(),
        name: input.name.clone(),
        slug: input.slug.clone(),
        api_key: api_key.clone(),
        created_at: now,
        created_by: session_user.map(|su| su.0.user_id),
    };

    let p = project.clone();
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    conn.interact(move |conn| {
        conn.execute(
            "INSERT INTO projects (id, name, slug, api_key, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![p.id, p.name, p.slug, p.api_key, p.created_at, p.created_by],
        )
    })
    .await
    .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
    .map_err(|e| {
        if e.to_string().contains("UNIQUE constraint failed") {
            AppError::Validation("a project with that slug already exists".to_string())
        } else {
            AppError::Database(e)
        }
    })?;

    Ok(Json(project))
}

/// GET /v1/projects/:slug - Get project details (without API key).
pub async fn get_project(
    State(pool): State<Arc<Pool>>,
    Path(slug): Path<String>,
) -> AppResult<Json<ProjectSummary>> {
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let project = conn
        .interact(move |conn| {
            conn.query_row(
                "SELECT id, name, slug, created_at, created_by
                 FROM projects WHERE slug = ?1",
                params![slug],
                |row| {
                    Ok(ProjectSummary {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        slug: row.get(2)?,
                        created_at: row.get(3)?,
                        created_by: row.get(4)?,
                    })
                },
            )
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                AppError::NotFound("project not found".to_string())
            }
            _ => AppError::Database(e),
        })?;

    Ok(Json(project))
}

/// PUT /v1/projects/:slug - Update project.
pub async fn update_project(
    State(pool): State<Arc<Pool>>,
    Path(slug): Path<String>,
    Json(input): Json<UpdateProject>,
) -> AppResult<Json<ProjectSummary>> {
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let s = slug.clone();
    let name = input.name.clone();

    let project = conn
        .interact(move |conn| {
            if let Some(ref name) = name {
                conn.execute(
                    "UPDATE projects SET name = ?1 WHERE slug = ?2",
                    params![name, s],
                )?;
            }
            conn.query_row(
                "SELECT id, name, slug, created_at, created_by
                 FROM projects WHERE slug = ?1",
                params![s],
                |row| {
                    Ok(ProjectSummary {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        slug: row.get(2)?,
                        created_at: row.get(3)?,
                        created_by: row.get(4)?,
                    })
                },
            )
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                AppError::NotFound("project not found".to_string())
            }
            _ => AppError::Database(e),
        })?;

    Ok(Json(project))
}

/// DELETE /v1/projects/:slug - Delete a project.
pub async fn delete_project(
    State(pool): State<Arc<Pool>>,
    Path(slug): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    // Prevent deleting the default project
    if slug == "default" {
        return Err(AppError::Validation(
            "cannot delete the default project".to_string(),
        ));
    }

    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let deleted = conn
        .interact(move |conn| conn.execute("DELETE FROM projects WHERE slug = ?1", params![slug]))
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    if deleted == 0 {
        return Err(AppError::NotFound("project not found".to_string()));
    }

    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// POST /v1/projects/:slug/rotate-key - Rotate API key.
pub async fn rotate_key(
    State(pool): State<Arc<Pool>>,
    Path(slug): Path<String>,
) -> AppResult<Json<Project>> {
    let new_key = generate_api_key();
    let conn = pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let key = new_key.clone();
    let s = slug.clone();
    let project = conn
        .interact(move |conn| {
            let updated = conn.execute(
                "UPDATE projects SET api_key = ?1 WHERE slug = ?2",
                params![key, s],
            )?;
            if updated == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            conn.query_row(
                "SELECT id, name, slug, api_key, created_at, created_by
                 FROM projects WHERE slug = ?1",
                params![s],
                |row| {
                    Ok(Project {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        slug: row.get(2)?,
                        api_key: row.get(3)?,
                        created_at: row.get(4)?,
                        created_by: row.get(5)?,
                    })
                },
            )
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))?
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                AppError::NotFound("project not found".to_string())
            }
            _ => AppError::Database(e),
        })?;

    Ok(Json(project))
}

/// Generate a random API key: "bloop_" + 64 hex chars (256-bit entropy).
fn generate_api_key() -> String {
    let bytes: [u8; 32] = rand::random();
    format!("bloop_{}", hex::encode(bytes))
}
