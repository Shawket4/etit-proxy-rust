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

        // Report server faults. This is the one funnel every error response
        // passes through, so capturing here covers the whole surface — the
        // tower layer only binds a hub, it does not capture anything itself.
        //
        // 4xx is skipped: a caller sending a bad request is not a fault here.
        if status.is_server_error() {
            match &self {
                // The body of an upstream response is where the upstream's own
                // data lives, and it is embedded in this variant's Display.
                // The scrubber redacts by KEY and cannot clean a message, so
                // this one is reported by shape only — never by Display.
                AppError::UpstreamStatus { status: upstream, .. } => {
                    crate::observability::capture_upstream_status("etit", "http", *upstream);
                }
                AppError::Upstream(e) => {
                    crate::observability::capture_upstream_failure("etit", "transport", None, e);
                }
                AppError::SessionDead => {
                    crate::observability::capture_upstream_status("etit", "session", 401);
                }
                // Same hazard as UpstreamStatus above, reached by a path that
                // bypassed the guard. `login()` puts 400 characters of the
                // login page body -- and the cookies from the exchange, values
                // included -- into its message, which then arrives here as an
                // opaque String. Reported by shape only, never by Display.
                AppError::Login(_) => {
                    crate::observability::capture_opaque_failure("etit", "login");
                }
                other => {
                    sentry::capture_error(other);
                }
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Compliance control. `login()` builds its message out of the login page
    /// body and the cookies from the exchange, so this variant must reach
    /// Sentry by shape alone. The scrubber redacts by key and cannot clean a
    /// free-text message, so this funnel is the only thing standing between a
    /// session token and the issue tracker. Do not relax it.
    #[test]
    fn login_failures_are_reported_without_their_message() {
        let events = sentry::test::with_captured_events(|| {
            let _ = AppError::Login(
                "login POST returned 200: SSOCookie=super-secret-token".into(),
            )
            .into_response();
        });

        assert_eq!(events.len(), 1, "a 502 must still raise an issue");
        let dumped = serde_json::to_string(&events[0]).unwrap();
        assert!(!dumped.contains("super-secret-token"), "leaked a cookie value: {dumped}");
        assert!(!dumped.contains("SSOCookie"), "leaked a cookie name: {dumped}");
    }

    /// The same guard applied to the sibling variant that always had it.
    #[test]
    fn upstream_bodies_are_reported_without_their_message() {
        let events = sentry::test::with_captured_events(|| {
            let _ = AppError::UpstreamStatus {
                status: 500,
                body: "driver=Mohamed&plate=123".into(),
            }
            .into_response();
        });

        assert_eq!(events.len(), 1);
        let dumped = serde_json::to_string(&events[0]).unwrap();
        assert!(!dumped.contains("Mohamed"), "leaked an upstream body: {dumped}");
    }

    /// And the other half: quieting the leak must not quiet the fault.
    #[test]
    fn ordinary_server_faults_still_raise_an_issue() {
        let events = sentry::test::with_captured_events(|| {
            let _ = AppError::Internal(anyhow::anyhow!("disk on fire")).into_response();
        });
        assert_eq!(events.len(), 1);
    }

    /// A caller sending nonsense is not a fault here, and must stay silent.
    #[test]
    fn client_errors_are_not_reported() {
        let events = sentry::test::with_captured_events(|| {
            let _ = AppError::BadRequest("no such date".into()).into_response();
            let _ = AppError::NotFound("vehicle".into()).into_response();
            let _ = AppError::Unauthorized.into_response();
        });
        assert!(events.is_empty(), "4xx must not raise issues: {events:?}");
    }
}
