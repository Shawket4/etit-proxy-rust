//! HTTP API.
//!
//! All public endpoints live under `/api/v1/*`. Anything else
//! (`/healthz`, `/metrics`, `/admin/*`) is operational and explicitly
//! versionless — its contract is implementation-defined and subject to
//! change without notice.

pub mod dto;
pub mod handlers;
pub mod middleware;

use std::str::FromStr;

use axum::http::{header, HeaderName, HeaderValue, Method};
use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    let cors = build_cors(&state);

    // API v1 — stable contract.
    let v1 = Router::new()
        .route("/vehicles",                get(handlers::vehicles::list))
        .route("/vehicles/live",           get(handlers::live::list))
        .route("/vehicles/:id",            get(handlers::vehicles::get_one))
        .route("/vehicles/:id/live",       get(handlers::live::get_one))
        .route("/vehicles/:id/history",         get(handlers::history::for_vehicle))
        .route("/vehicles/:id/history/summary", get(handlers::history::summary))
        .route("/stream/live",                  get(handlers::stream::live_sse))
        .route("/zones",                        get(handlers::zones::list).post(handlers::zones::create))
        .route("/zones/:id",                    get(handlers::zones::get_one).put(handlers::zones::update).delete(handlers::zones::delete))
        .route("/zones/scan",                   post(handlers::zones::run_scan_now))
        .route("/trip-audit/matches",           get(handlers::trip_audit::list_matches))
        .route("/trip-audit/matches/:id",       get(handlers::trip_audit::get_match))
        .route("/trip-audit/matches/:id/review", post(handlers::trip_audit::review_match))
        .route("/trip-audit/runs",              get(handlers::trip_audit::list_runs))
        .route("/trip-audit/scan",              post(handlers::trip_audit::run_scan_now))
        .route("/trip-audit/suggestions",       get(handlers::trip_audit::list_suggestions))
        .route("/trip-audit/suggestions/:id/ack", post(handlers::trip_audit::ack_suggestion));

    // Admin — gated by token. If admin is disabled, the middleware short-
    // circuits with a 404 so the surface area is invisible.
    let admin_state = state.clone();
    let admin = Router::new()
        .route("/cache/invalidate", post(handlers::admin::invalidate))
        .route("/auth/relogin",     post(handlers::admin::relogin))
        .route("/users/unblock",    post(handlers::admin::unblock_user))
        .layer(from_fn_with_state(admin_state, middleware::admin_guard));

    Router::new()
        .nest("/api/v1", v1)
        .nest("/admin",  admin)
        .route("/healthz", get(handlers::health::healthz))
        .route("/metrics", get(handlers::metrics::metrics))
        .with_state(state)
        // Apply layers from outermost-in. `request_id` runs last so it can
        // tag every log line that follows.
        .layer(cors)
        .layer(CompressionLayer::new())
        .layer(no_store_for_api())
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(middleware::request_id))
}

/// Sends `Cache-Control: no-store` on every response, which is what we want
/// for an API: caches must never serve stale snapshots back to a browser.
fn no_store_for_api() -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-store"),
    )
}

fn build_cors(state: &AppState) -> CorsLayer {
    let allow_origin = if state.cfg.cors_origins.iter().any(|s| s == "*") {
        AllowOrigin::any()
    } else {
        let origins: Vec<HeaderValue> = state
            .cfg
            .cors_origins
            .iter()
            .filter_map(|s| HeaderValue::from_str(s).ok())
            .collect();
        if origins.is_empty() {
            AllowOrigin::any()
        } else {
            AllowOrigin::list(origins)
        }
    };

    CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::ACCEPT,
            HeaderName::from_str("x-admin-token").unwrap(),
        ])
}
