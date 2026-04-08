//! Application error types.
//!
//! Provides structured error responses with machine-readable codes,
//! human-readable messages, request correlation IDs, and per-field
//! validation details. API requests receive JSON; HTML error pages
//! are rendered by separate helpers in `routes::helpers`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Structured error response returned to API clients.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    /// Machine-readable error code (e.g., "validation_failed", "not_found").
    pub code: &'static str,
    /// Human-readable message safe for display to end users.
    pub message: String,
    /// Unique request correlation ID for log tracing.
    pub request_id: String,
    /// Optional field-level details (for validation errors).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Vec<FieldError>>,
}

/// Per-field validation error.
#[derive(Debug, Clone, Serialize)]
pub struct FieldError {
    /// Field name that failed validation.
    pub field: String,
    /// Machine-readable error code (e.g., "required", "too_long", "invalid_format").
    pub code: &'static str,
    /// Human-readable description of what's wrong.
    pub message: String,
}

/// Application error with structured context.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    // --- Client errors (4xx) ---
    /// Content, user, or resource not found.
    #[error("{entity} not found")]
    NotFound {
        entity: &'static str,
        id: Option<String>,
    },

    /// Authentication required or credentials invalid.
    #[error("unauthorized: {reason}")]
    Unauthorized { reason: String },

    /// Authenticated but insufficient permissions.
    #[error("forbidden: {reason}")]
    Forbidden { reason: String },

    /// Input validation failed with field-level details.
    #[error("validation failed")]
    Validation { errors: Vec<FieldError> },

    /// General bad request (malformed JSON, missing header, etc.).
    #[error("bad request: {message}")]
    BadRequest { message: String },

    /// Resource conflict (duplicate, already exists, concurrent edit).
    #[error("conflict: {message}")]
    Conflict { message: String },

    /// Rate limit exceeded.
    #[error("rate limit exceeded")]
    RateLimited {
        retry_after_secs: u64,
        category: String,
    },

    /// Request payload too large.
    #[error("payload too large")]
    PayloadTooLarge { max_bytes: u64 },

    // --- Server errors (5xx) ---
    /// Database error — logged with full details, user sees classified message.
    #[error("database error")]
    Database {
        #[source]
        source: sqlx::Error,
        operation: &'static str,
    },

    /// External service unavailable (AI, SMTP, S3).
    #[error("service unavailable: {service}")]
    ServiceUnavailable {
        service: &'static str,
        reason: String,
    },

    /// Internal error — catch-all for unexpected failures.
    #[error("internal error")]
    Internal {
        #[source]
        source: anyhow::Error,
        context: Option<String>,
    },

    /// Plugin execution error.
    #[error("plugin error: {plugin}")]
    Plugin { plugin: String, message: String },
}

// =========================================================================
// Convenience constructors
// =========================================================================

impl AppError {
    /// Resource not found (no ID).
    pub fn not_found(entity: &'static str) -> Self {
        Self::NotFound { entity, id: None }
    }

    /// Resource not found with a specific ID.
    pub fn not_found_id(entity: &'static str, id: impl ToString) -> Self {
        Self::NotFound {
            entity,
            id: Some(id.to_string()),
        }
    }

    /// Authentication required.
    pub fn unauthorized(reason: impl Into<String>) -> Self {
        Self::Unauthorized {
            reason: reason.into(),
        }
    }

    /// Permission denied.
    pub fn forbidden(reason: impl Into<String>) -> Self {
        Self::Forbidden {
            reason: reason.into(),
        }
    }

    /// Bad request (not a validation error).
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest {
            message: message.into(),
        }
    }

    /// Resource conflict.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict {
            message: message.into(),
        }
    }

    /// Database error with operation context.
    pub fn db(source: sqlx::Error, operation: &'static str) -> Self {
        Self::Database { source, operation }
    }

    /// External service unavailable.
    pub fn service_unavailable(service: &'static str, reason: impl Into<String>) -> Self {
        Self::ServiceUnavailable {
            service,
            reason: reason.into(),
        }
    }

    /// Internal error from any source.
    pub fn internal(source: impl Into<anyhow::Error>) -> Self {
        Self::Internal {
            source: source.into(),
            context: None,
        }
    }

    /// Internal error with additional context.
    pub fn internal_ctx(source: impl Into<anyhow::Error>, context: impl Into<String>) -> Self {
        Self::Internal {
            source: source.into(),
            context: Some(context.into()),
        }
    }

    /// Validation error with per-field details.
    pub fn validation(errors: Vec<FieldError>) -> Self {
        Self::Validation { errors }
    }

    /// Create a single field error.
    pub fn field_error(
        field: impl Into<String>,
        code: &'static str,
        message: impl Into<String>,
    ) -> FieldError {
        FieldError {
            field: field.into(),
            code,
            message: message.into(),
        }
    }
}

// =========================================================================
// From implementations — preserve backward compatibility with `?` operator
// =========================================================================

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal {
            source: err,
            context: None,
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        if matches!(err, sqlx::Error::RowNotFound) {
            return Self::NotFound {
                entity: "record",
                id: None,
            };
        }
        Self::Database {
            source: err,
            operation: "query",
        }
    }
}

// =========================================================================
// HTTP response conversion
// =========================================================================

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let request_id = uuid::Uuid::now_v7().to_string();

        let (status, code, message, details) = match &self {
            AppError::NotFound { entity, id } => {
                let msg = match id {
                    Some(id) => format!("{entity} '{id}' not found"),
                    None => format!("{entity} not found"),
                };
                (StatusCode::NOT_FOUND, "not_found", msg, None)
            }
            AppError::Unauthorized { reason } => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                reason.clone(),
                None,
            ),
            AppError::Forbidden { reason } => {
                (StatusCode::FORBIDDEN, "forbidden", reason.clone(), None)
            }
            AppError::Validation { errors } => {
                let msg = format!("{} validation error(s)", errors.len());
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "validation_failed",
                    msg,
                    Some(errors.clone()),
                )
            }
            AppError::BadRequest { message } => (
                StatusCode::BAD_REQUEST,
                "bad_request",
                message.clone(),
                None,
            ),
            AppError::Conflict { message } => {
                (StatusCode::CONFLICT, "conflict", message.clone(), None)
            }
            AppError::RateLimited {
                retry_after_secs,
                category,
            } => {
                let msg =
                    format!("Rate limit exceeded for {category}. Retry after {retry_after_secs}s.");
                (StatusCode::TOO_MANY_REQUESTS, "rate_limited", msg, None)
            }
            AppError::PayloadTooLarge { max_bytes } => {
                let msg = format!("Payload exceeds maximum size of {max_bytes} bytes");
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "payload_too_large",
                    msg,
                    None,
                )
            }
            AppError::Database {
                source, operation, ..
            } => {
                let msg = classify_db_error(source, operation);
                let db_status = classify_db_status(source);
                tracing::error!(
                    error = %source,
                    operation = operation,
                    request_id = %request_id,
                    "Database error"
                );
                (db_status, "database_error", msg, None)
            }
            AppError::ServiceUnavailable { service, reason } => {
                tracing::warn!(service = service, reason = reason, "Service unavailable");
                let msg = format!("{service} is temporarily unavailable");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "service_unavailable",
                    msg,
                    None,
                )
            }
            AppError::Internal { source, context } => {
                tracing::error!(
                    error = %source,
                    context = context.as_deref().unwrap_or("none"),
                    request_id = %request_id,
                    "Internal error"
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "An unexpected error occurred".to_string(),
                    None,
                )
            }
            AppError::Plugin { plugin, message } => {
                tracing::error!(plugin = plugin, message = message, "Plugin error");
                let msg = format!("Plugin '{plugin}' encountered an error");
                (StatusCode::INTERNAL_SERVER_ERROR, "plugin_error", msg, None)
            }
        };

        let body = ErrorResponse {
            code,
            message,
            request_id,
            details,
        };

        let mut response = (status, Json(body)).into_response();

        // Add Retry-After header for rate limiting
        if let AppError::RateLimited {
            retry_after_secs, ..
        } = &self
        {
            // Infallible: numeric string is always a valid header value
            response.headers_mut().insert(
                "Retry-After",
                axum::http::HeaderValue::from(*retry_after_secs),
            );
        }

        response
    }
}

// =========================================================================
// Database error classification
// =========================================================================

/// Inspect `sqlx::Error` for known PostgreSQL error codes and return a
/// user-facing message instead of "internal server error".
fn classify_db_error(err: &sqlx::Error, operation: &str) -> String {
    match err {
        sqlx::Error::Database(db_err) => match db_err.code().as_deref() {
            Some("23505") => {
                let detail = db_err.message();
                format!("A record with that value already exists: {detail}")
            }
            Some("23503") => "Cannot delete: this record is referenced by other records".into(),
            Some("23502") => "A required field is missing".into(),
            Some("23514") => "A value violates a constraint".into(),
            Some("22001") => "A value is too long for its field".into(),
            _ => format!("Database {operation} failed"),
        },
        sqlx::Error::PoolTimedOut => "Database is busy, please try again".into(),
        sqlx::Error::PoolClosed => "Database is unavailable".into(),
        _ => format!("Database {operation} failed"),
    }
}

/// Map known database errors to appropriate HTTP status codes.
fn classify_db_status(err: &sqlx::Error) -> StatusCode {
    match err {
        sqlx::Error::Database(db_err) => match db_err.code().as_deref() {
            Some("23505") => StatusCode::CONFLICT,
            Some("23503") => StatusCode::CONFLICT,
            Some("23502") => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        },
        sqlx::Error::RowNotFound => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

// =========================================================================
// Result type alias
// =========================================================================

/// Result type alias using AppError.
pub type AppResult<T> = Result<T, AppError>;

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn not_found_without_id() {
        let err = AppError::not_found("item");
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn not_found_with_id() {
        let err = AppError::not_found_id("item", "abc-123");
        assert!(err.to_string().contains("item not found"));
    }

    #[test]
    fn validation_error_status() {
        let err = AppError::validation(vec![AppError::field_error(
            "email",
            "required",
            "Email is required",
        )]);
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn bad_request_status() {
        let err = AppError::bad_request("malformed JSON");
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn unauthorized_status() {
        let err = AppError::unauthorized("login required");
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn forbidden_status() {
        let err = AppError::forbidden("no permission");
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn conflict_status() {
        let err = AppError::conflict("already exists");
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn internal_from_anyhow() {
        let anyhow_err = anyhow::anyhow!("something broke");
        let err: AppError = anyhow_err.into();
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn sqlx_row_not_found_becomes_not_found() {
        let sqlx_err = sqlx::Error::RowNotFound;
        let err: AppError = sqlx_err.into();
        assert!(matches!(err, AppError::NotFound { .. }));
    }

    #[test]
    fn rate_limited_has_retry_after_header() {
        let err = AppError::RateLimited {
            retry_after_secs: 60,
            category: "api".to_string(),
        };
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok()),
            Some("60")
        );
    }

    #[test]
    fn classify_unique_violation_is_conflict() {
        // PostgreSQL error code 23505 = unique_violation
        assert_eq!(
            classify_db_status(&sqlx::Error::RowNotFound),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn pool_timeout_message() {
        let msg = classify_db_error(&sqlx::Error::PoolTimedOut, "query");
        assert!(msg.contains("busy"));
    }

    #[test]
    fn service_unavailable_status() {
        let err = AppError::service_unavailable("SMTP", "connection refused");
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn payload_too_large_status() {
        let err = AppError::PayloadTooLarge {
            max_bytes: 10_000_000,
        };
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn plugin_error_status() {
        let err = AppError::Plugin {
            plugin: "test_plugin".to_string(),
            message: "tap failed".to_string(),
        };
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
