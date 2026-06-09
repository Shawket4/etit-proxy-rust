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
            Err(e) => tracing::warn!(error = %e, "tier B refresh failed"),
        }
    }
}
