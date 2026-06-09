//! Middleware for the API.
//!
//! - **Request ID** — generates or propagates a UUID and echoes it on the
//!   response. The id is also placed in a tracing span so every log line
//!   under that request carries it.
//! - **Admin guard** — if `cfg.admin_token` is unset, all `/admin/*` routes
//!   are 404'd. Otherwise they require an `X-Admin-Token` header that
//!   matches the configured value.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;

const REQUEST_ID_HEADER: &str = "x-request-id";
const ADMIN_HEADER: &str = "x-admin-token";

/// Generates `X-Request-Id` if missing, attaches it to the request
/// extensions for downstream code to read, and echoes it on the response.
pub async fn request_id(mut req: axum::extract::Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    if let Ok(hv) = HeaderValue::from_str(&id) {
        req.headers_mut()
            .insert(HeaderName::from_static(REQUEST_ID_HEADER), hv.clone());
        let _ = req.extensions_mut().insert(RequestId(id.clone()));
        let span = tracing::info_span!("req", id = %id);
        let _entered = span.enter();
        let mut resp = next.run(req).await;
        resp.headers_mut()
            .insert(HeaderName::from_static(REQUEST_ID_HEADER), hv);
        return resp;
    }
    next.run(req).await
}

#[derive(Debug, Clone)]
pub struct RequestId(pub String);

pub async fn admin_guard(
    State(s): State<AppState>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(expected) = s.cfg.admin_token.as_deref() else {
        // Admin disabled entirely.
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    };
    let provided = req
        .headers()
        .get(ADMIN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if provided.is_empty() || !constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "admin token required", "code": "UNAUTHORIZED"})),
        )
            .into_response();
    }
    next.run(req).await
}

/// Constant-time byte comparison. Length mismatch short-circuits to false.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Helper for handlers that want to read the request id from a [`HeaderMap`].
#[allow(dead_code)]
pub fn extract_request_id(headers: &HeaderMap) -> Option<&str> {
    headers.get(REQUEST_ID_HEADER).and_then(|v| v.to_str().ok())
}
