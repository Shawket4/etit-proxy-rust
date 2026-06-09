pub mod claims;
pub use claims::Claims;

use axum::{
    body::Body,
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use jsonwebtoken::{decode, DecodingKey, Validation};

/// Axum middleware — validates JWT on every request except those that bypass it.
///
/// Token lookup order (mirrors the Rust/actix service):
///   1. `jwt` cookie
///   2. `Authorization: Bearer <token>` header
///
/// On success the validated `Claims` are inserted into request extensions so
/// any handler downstream can extract them via `Extension<Claims>`.
pub async fn require_admin(
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let jwt_secret = std::env::var("JWT_SECRET")
        .expect("JWT_SECRET must be set");

    // ── 1. Extract raw token ──────────────────────────────────────────────
    let token = extract_token(&req);

    let token = match token {
        Some(t) => t,
        None => {
            return (StatusCode::UNAUTHORIZED, "Not authenticated").into_response();
        }
    };

    // ── 2. Decode & verify signature + expiry ────────────────────────────
    let token_data = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(jwt_secret.as_bytes()),
        &Validation::default(),
    );

    let claims = match token_data {
        Ok(data) => data.claims,
        Err(err) => {
            tracing::warn!(error = %err, "JWT validation failed");
            return (StatusCode::UNAUTHORIZED, "Invalid token").into_response();
        }
    };

    // ── 3. Admin gate ────────────────────────────────────────────────────
    if !claims.is_admin() {
        tracing::warn!(
            user_type = %claims.user_type,
            "Non-admin attempted to access etit-proxy"
        );
        return (StatusCode::FORBIDDEN, "Admin access required").into_response();
    }

    // ── 4. Forward claims to handlers ────────────────────────────────────
    req.extensions_mut().insert(claims);
    next.run(req).await
}

/// Pulls the raw JWT string from either the `jwt` cookie or the
/// `Authorization: Bearer` header, preferring the cookie.
fn extract_token(req: &Request<Body>) -> Option<String> {
    // Cookie first
    if let Some(cookie_header) = req.headers().get(header::COOKIE) {
        if let Ok(cookie_str) = cookie_header.to_str() {
            for part in cookie_str.split(';') {
                let part = part.trim();
                if let Some(value) = part.strip_prefix("jwt=") {
                    return Some(value.to_string());
                }
            }
        }
    }

    // Authorization: Bearer fallback
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|t| t.to_string())
}