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
            Err(e)   => tracing::warn!(error = %e, "tier A refresh failed"),
        }
    }
}
