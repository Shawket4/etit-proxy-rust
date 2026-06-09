//! Tier C — hot-history poller.
//!
//! Every `tier_c_poll` interval:
//!   1. Cool any DB rows whose `is_hot` flag is no longer accurate (the
//!      calendar may have rolled over since the previous tick).
//!   2. For every known vehicle, refresh today + yesterday in parallel,
//!      bounded by `tier_c_concurrency`.

use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::time::{interval, MissedTickBehavior};

use crate::domain::time::today_local;

use super::Pollers;

pub async fn run(p: Arc<Pollers>) {
    let mut tick = interval(p.cfg.tier_c_poll);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let sem = Arc::new(Semaphore::new(p.cfg.tier_c_concurrency.max(1)));

    loop {
        tick.tick().await;

        let tz = p.cfg.timezone;
        let today = today_local(tz);
        let yesterday = today.pred_opt().unwrap_or(today);

        // Cool stale hot rows in the DB. Failure here is non-fatal —
        // cool/hot is a polling hint, not a correctness invariant.
        if let Err(e) = p.history_repo.cool_aged_hot_rows(today, yesterday).await {
            tracing::warn!(error = %e, "cool_aged_hot_rows failed");
        }

        // Snapshot the vehicle list for this tick.
        let vehicles = match p.vehicles.list().await {
            Ok(v) if !v.is_empty() => v,
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "tier C: could not list vehicles");
                continue;
            }
        };

        // Fan out concurrently, capped by the semaphore.
        let mut handles = Vec::with_capacity(vehicles.len() * 2);
        for v in vehicles.iter() {
            for date in [today, yesterday] {
                let svc = p.history.clone();
                let sem = sem.clone();
                let vid = v.id.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = match sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => return,
                    };
                    if let Err(e) = svc.for_day(&vid, date, true).await {
                        tracing::debug!(vehicle = %vid, %date, error = %e, "tier C day refresh failed");
                    }
                }));
            }
        }

        // Wait for the entire round to finish before the next tick.
        for h in handles {
            let _ = h.await;
        }
        tracing::info!(
            vehicles = vehicles.len(),
            "tier C refresh completed (today + yesterday)"
        );
    }
}
