//! `/metrics` — JSON metrics dump.
//!
//! Deliberately not Prometheus-formatted; pulled into ops dashboards via
//! straight HTTP fetch. If you want Prometheus output, scrape this and
//! convert downstream.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::cache::CacheStats;
use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsResponse {
    pub uptime_secs: u64,
    pub cache: CacheStats,
    pub auth_epoch: u64,
    pub auth_ready: bool,
    pub sse_subscribers: usize,
    pub config: ConfigMirror,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigMirror {
    pub timezone: String,
    pub tier_a_refresh_secs: u64,
    pub tier_b_poll_secs: u64,
    pub tier_c_poll_secs: u64,
    pub tier_c_concurrency: usize,
    pub upstream_timeout_secs: u64,
    pub upstream_retry_max: u32,
    pub admin_enabled: bool,
}

pub async fn metrics(State(s): State<AppState>) -> Json<MetricsResponse> {
    let auth = s.client.auth_snapshot().await;
    Json(MetricsResponse {
        uptime_secs: s.started_at.elapsed().as_secs(),
        cache: s.cache.stats(),
        auth_epoch: auth.epoch,
        auth_ready: auth.is_ready(),
        sse_subscribers: s.broadcaster.subscriber_count(),
        config: ConfigMirror {
            timezone: s.cfg.timezone.to_string(),
            tier_a_refresh_secs: s.cfg.tier_a_refresh.as_secs(),
            tier_b_poll_secs:    s.cfg.tier_b_poll.as_secs(),
            tier_c_poll_secs:    s.cfg.tier_c_poll.as_secs(),
            tier_c_concurrency:  s.cfg.tier_c_concurrency,
            upstream_timeout_secs: s.cfg.upstream_timeout.as_secs(),
            upstream_retry_max:  s.cfg.upstream_retry_max,
            admin_enabled:       s.cfg.admin_enabled(),
        },
    })
}
