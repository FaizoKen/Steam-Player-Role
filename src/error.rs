use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Steam API error: {0}")]
    SteamApi(String),

    #[error("Steam API budget exhausted; retry in {retry_after_secs}s")]
    QuotaExhausted { retry_after_secs: u64 },

    #[error("RoleLogic API error: {0}")]
    RoleLogic(String),

    #[error("Role link not found on RoleLogic")]
    RoleLinkNotFound,

    #[error("Role link is disabled on RoleLogic")]
    RoleLinkDisabled,

    #[error("Role link user limit reached ({limit})")]
    UserLimitReached { limit: usize },

    #[error("Invalid request: {0}")]
    BadRequest(String),

    #[error("Too many requests: {0}")]
    TooManyRequests(String),

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Unauthorized: {0}")]
    UnauthorizedWith(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Configuration was changed in another tab")]
    StaleVersion,

    #[error("Verification failed: {0}")]
    VerificationFailed(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Database(e) => {
                tracing::error!("Database error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            }
            AppError::SteamApi(e) => {
                tracing::error!("Steam API error: {e}");
                (
                    StatusCode::BAD_GATEWAY,
                    "Failed to fetch Steam data. Please try again later.",
                )
            }
            AppError::QuotaExhausted { retry_after_secs } => {
                tracing::warn!(retry_after_secs, "Steam API budget exhausted");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Steam data checks are temporarily rate-limited. Please try again later.",
                )
            }
            AppError::RoleLogic(e) => {
                tracing::error!("RoleLogic API error: {e}");
                (StatusCode::BAD_GATEWAY, "Failed to sync roles")
            }
            AppError::RoleLinkNotFound => (StatusCode::NOT_FOUND, "Role link not found"),
            AppError::RoleLinkDisabled => (StatusCode::FORBIDDEN, "Role link is disabled"),
            AppError::UserLimitReached { limit } => {
                tracing::warn!("Role link user limit reached: {limit}");
                (StatusCode::FORBIDDEN, "Role link user limit reached")
            }
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.as_str()),
            AppError::TooManyRequests(msg) => (StatusCode::TOO_MANY_REQUESTS, msg.as_str()),
            AppError::Unauthorized => {
                (StatusCode::UNAUTHORIZED, "Invalid or missing authorization")
            }
            AppError::UnauthorizedWith(msg) => (StatusCode::UNAUTHORIZED, msg.as_str()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.as_str()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.as_str()),
            AppError::StaleVersion => (
                StatusCode::CONFLICT,
                "This configuration was changed in another tab. Reload to get the latest, then re-apply your edit.",
            ),
            AppError::VerificationFailed(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg.as_str()),
            AppError::Internal(e) => {
                tracing::error!("Internal error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            }
        };

        let body = json!({ "error": message });
        (status, axum::Json(body)).into_response()
    }
}
