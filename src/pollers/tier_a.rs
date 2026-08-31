//! Tier A — periodic vehicle metadata refresh.

use std::sync::Arc;

use tokio::time::{interval, MissedTickBehavior};

use super::Pollers;

pub async fn run(p: Arc<Pollers>) {
    let mut tick = interval(p.cfg.tier_a_refresh);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match p.vehicles.refresh_now().await {
            Ok(list) => tracing::info!(count = list.len(), "tier A refreshed"),
            Err(e) => {
                // Report, don't just log. A background refresh failing IS the
                // upstream being down, and it is the only place we would see an
                // ETIT outage — no request is in flight to carry it. A warn
                // becomes a breadcrumb and raises no issue, so the whole outage
                // would have been silent.
                tracing::warn!(error = %e, "poller.tier_a refresh failed");
                crate::observability::capture_upstream_failure(
                    "etit", "poller.tier_a", None, &e,
                );
            }
        }
    }
}
