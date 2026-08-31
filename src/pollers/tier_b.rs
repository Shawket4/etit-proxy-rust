//! Tier B — live-status poller.
//!
//! Refreshes the live snapshot every minute (configurable). After each
//! successful refresh, publishes the new snapshot on the broadcaster so SSE
//! subscribers get a push.

use std::sync::Arc;

use tokio::time::{interval, MissedTickBehavior};

use crate::services::broadcaster::LiveEvent;

use super::Pollers;

pub async fn run(p: Arc<Pollers>) {
    let mut tick = interval(p.cfg.tier_b_poll);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match p.live.refresh_now().await {
            Ok(snap) => {
                let count = snap.by_id.len();
                p.broadcaster.publish(LiveEvent::Update(snap));
                tracing::debug!(count, "tier B refreshed");
            }
            Err(e) => {
                // Report, don't just log. A background refresh failing IS the
                // upstream being down, and it is the only place we would see an
                // ETIT outage — no request is in flight to carry it. A warn
                // becomes a breadcrumb and raises no issue, so the whole outage
                // would have been silent.
                tracing::warn!(error = %e, "poller.tier_b refresh failed");
                crate::observability::capture_upstream_failure(
                    "etit", "poller.tier_b", None, &e,
                );
            }
        }
    }
}
