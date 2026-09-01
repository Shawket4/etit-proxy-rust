//! Tier C — hot-history poller.
//!
//! Every `tier_c_poll` interval:
//!   1. Cool any DB rows whose `is_hot` flag is no longer accurate (the
//!      calendar may have rolled over since the previous tick).
//!   2. For every known vehicle, refresh today + yesterday in parallel,
//!      bounded by `tier_c_concurrency`.

use std::sync::atomic::{AtomicUsize, Ordering};
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
            // Still non-fatal to the tick, but a warn alone raises no issue, so
            // a database that has been rejecting this write for a week looks
            // exactly like one that never fails. Once per tick at most.
            tracing::warn!(error = %e, "cool_aged_hot_rows failed");
            crate::observability::capture_upstream_failure(
                "postgres", "poller.tier_c.cool_aged_hot_rows", None, e.as_ref(),
            );
        }

        // Snapshot the vehicle list for this tick.
        let vehicles = match p.vehicles.list().await {
            Ok(v) if !v.is_empty() => v,
            Ok(_) => continue,
            Err(e) => {
                // The same upstream outage tier A and B report, reached from a
                // third path. Silent here meant the outage was only two-thirds
                // visible.
                tracing::warn!(error = %e, "tier C: could not list vehicles");
                crate::observability::capture_upstream_failure(
                    "etit", "poller.tier_c.list_vehicles", None, &e,
                );
                continue;
            }
        };

        // Fan out concurrently, capped by the semaphore.
        //
        // A single vehicle failing is ordinary and stays at debug — the device
        // is off, or the day is empty. What is not ordinary is all of them
        // failing, which is the history pipeline being dead; counted here so
        // the round can say so once, rather than 2N times or never.
        let failed = Arc::new(AtomicUsize::new(0));
        let attempted = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::with_capacity(vehicles.len() * 2);
        for v in vehicles.iter() {
            for date in [today, yesterday] {
                let svc = p.history.clone();
                let sem = sem.clone();
                let vid = v.id.clone();
                let failed = failed.clone();
                let attempted = attempted.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = match sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => return,
                    };
                    attempted.fetch_add(1, Ordering::Relaxed);
                    if let Err(e) = svc.for_day(&vid, date, true).await {
                        failed.fetch_add(1, Ordering::Relaxed);
                        tracing::debug!(vehicle = %vid, %date, error = %e, "tier C day refresh failed");
                    }
                }));
            }
        }

        // Wait for the entire round to finish before the next tick.
        for h in handles {
            let _ = h.await;
        }

        let failed = failed.load(Ordering::Relaxed);
        let attempted = attempted.load(Ordering::Relaxed);
        if attempted > 0 && failed == attempted {
            crate::observability::capture_batch_failure(
                "poller.tier_c.day_refresh", failed, attempted,
            );
        }
        tracing::info!(
            vehicles = vehicles.len(),
            attempted,
            failed,
            "tier C refresh completed (today + yesterday)"
        );
    }
}
