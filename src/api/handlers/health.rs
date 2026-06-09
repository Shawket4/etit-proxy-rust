//! `/healthz` — operational endpoint.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::cache::CacheStats;
use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub uptime_secs: u64,
    pub auth_ready: bool,
    pub auth_epoch: u64,
    pub last_login_at: Option<crate::domain::Timestamp>,
    pub cache: CacheStats,
    pub sse_subscribers: usize,
}

pub async fn healthz(State(s): State<AppState>) -> Json<HealthResponse> {
    let auth = s.client.auth_snapshot().await;
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: s.started_at.elapsed().as_secs(),
        auth_ready: auth.is_ready(),
        auth_epoch: auth.epoch,
        last_login_at: auth.last_login_at,
        cache: s.cache.stats(),
        sse_subscribers: s.broadcaster.subscriber_count(),
    })
}
