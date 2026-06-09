//! Uniform error type. The HTTP layer maps each variant to:
//!   - a status code,
//!   - a stable string `code` the frontend can switch on,
//!   - a human-readable `error` message.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("upstream HTTP error: {0}")]
    Upstream(#[from] reqwest::Error),

    #[error("upstream returned status {status}: {body}")]
    UpstreamStatus { status: u16, body: String },

    #[error("login flow failed: {0}")]
    Login(String),

    #[error("session re-login failed after retry")]
    SessionDead,

    #[error("failed to parse upstream payload: {0}")]
    Parse(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("admin auth required")]
    Unauthorized,

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    pub fn http(&self) -> (StatusCode, &'static str) {
        use AppError::*;
        match self {
            BadRequest(_)              => (StatusCode::BAD_REQUEST,           "BAD_REQUEST"),
            NotFound(_)                => (StatusCode::NOT_FOUND,             "NOT_FOUND"),
            Unauthorized               => (StatusCode::UNAUTHORIZED,          "UNAUTHORIZED"),
            Login(_) | SessionDead     => (StatusCode::BAD_GATEWAY,           "SESSION_DEAD"),
            UpstreamStatus { .. }      => (StatusCode::BAD_GATEWAY,           "UPSTREAM_FAILED"),
            Upstream(_)                => (StatusCode::BAD_GATEWAY,           "UPSTREAM_FAILED"),
            Parse(_)                   => (StatusCode::BAD_GATEWAY,           "UPSTREAM_FAILED"),
            Db(_) | Internal(_)        => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = self.http();

        // Always log internally with the full chain.
        tracing::error!(error = %self, code, "request failed");

        // Public message: opaque for internal errors, otherwise the message.
        let public = match &self {
            AppError::Db(_) | AppError::Internal(_) => "internal server error".to_string(),
            AppError::Upstream(_) | AppError::UpstreamStatus { .. } | AppError::Parse(_) => {
                "upstream request failed".to_string()
            }
            AppError::Login(_) | AppError::SessionDead => {
                "upstream session could not be established".to_string()
            }
            other => other.to_string(),
        };

        let body = Json(json!({
            "error": public,
            "code":  code,
        }));
        (status, body).into_response()
    }
}

pub type AppResult<T> = std::result::Result<T, AppError>;
