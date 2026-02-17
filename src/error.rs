use axum::extract::rejection::JsonRejection;
use axum::extract::FromRequest;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("validation error: {0}")]
    Validation(String),

    #[error("authentication error: {0}")]
    #[allow(dead_code)]
    Auth(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("pool error: {0}")]
    Pool(#[from] deadpool_sqlite::InteractError),

    #[error("internal error: {0}")]
    Internal(String),

    #[cfg(feature = "analytics")]
    #[error("analytics error: {0}")]
    Analytics(String),

    #[cfg(feature = "llm-tracing")]
    #[error("llm tracing error: {0}")]
    LlmTracing(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Validation(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Auth(msg) => (StatusCode::UNAUTHORIZED, msg.clone()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::Database(e) => {
                tracing::error!(error = %e, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            AppError::Pool(e) => {
                tracing::error!(error = %e, "pool error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            AppError::Internal(msg) => {
                tracing::error!(error = %msg, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            #[cfg(feature = "analytics")]
            AppError::Analytics(msg) => {
                tracing::error!(error = %msg, "analytics error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            #[cfg(feature = "llm-tracing")]
            AppError::LlmTracing(msg) => {
                tracing::error!(error = %msg, "llm tracing error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
        };

        let body = json!({ "error": message });
        (status, axum::Json(body)).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

/// JSON extractor that logs deserialization errors (422s) before returning them.
/// Drop-in replacement for `axum::Json<T>`.
pub struct LoggedJson<T>(pub T);

impl<S, T> FromRequest<S> for LoggedJson<T>
where
    axum::Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(
        req: axum::extract::Request,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let path = req.uri().path().to_string();
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(LoggedJson(value)),
            Err(rejection) => {
                tracing::warn!(
                    path = %path,
                    status = 422,
                    error = %rejection,
                    "JSON parse error (client sent malformed payload)"
                );
                Err(AppError::Validation(rejection.body_text()))
            }
        }
    }
}
