//! Application error types.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

/// Application errors.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("internal server error")]
    Internal(#[from] anyhow::Error),

    #[error("not found")]
    NotFound,

    #[error("unauthorized")]
    Unauthorized,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("database error")]
    Database(#[from] sqlx::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };

        // In development, include error details; in production, be vague
        let body = match &self {
            AppError::Internal(e) => {
                tracing::error!(error = %e, "internal server error");
                "internal server error".to_string()
            }
            AppError::Database(e) => {
                tracing::error!(error = %e, "database error");
                "internal server error".to_string()
            }
            _ => self.to_string(),
        };

        (status, body).into_response()
    }
}

/// Result type alias using AppError.
pub type AppResult<T> = Result<T, AppError>;
