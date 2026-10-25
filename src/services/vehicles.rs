//! Vehicles service.
//!
//! Read order:
//!   1. memory cache, if fresh
//!   2. memory cache, even if stale (best-effort)
//!   3. upstream — and only if everything else is empty do we go here
//!      synchronously; under normal operation the Tier A poller refreshes
//!      well before the read TTL elapses.
//!
//! Writes go to memory + DB so a restart immediately serves stale-but-useful
//! data while the first Tier A tick runs.

use std::sync::Arc;

use crate::cache::CacheLayer;
use crate::db::VehiclesRepo;
use crate::domain::Vehicle;
use crate::error::AppError;
use crate::upstream::{vehicles as up, EtitClient};

pub struct VehiclesService {
    pub client: Arc<EtitClient>,
    pub cache: Arc<CacheLayer>,
    pub repo: VehiclesRepo,
}

impl VehiclesService {
    pub async fn list(&self) -> Result<Arc<Vec<Vehicle>>, AppError> {
        // Fresh hit — return immediately.
        if self.cache.vehicles_is_fresh() {
            return Ok(self.cache.vehicles_snapshot().vehicles);
        }

        // Stale hit — return what we have; let the poller refresh in background.
        let snap = self.cache.vehicles_snapshot();
        if !snap.vehicles.is_empty() {
            return Ok(snap.vehicles);
        }

        // Cold start: hit upstream synchronously, then store everywhere.
        self.refresh_now().await
    }

    pub async fn get_by_id(&self, id: &str) -> Result<Option<Vehicle>, AppError> {
        let list = self.list().await?;
        Ok(list.iter().find(|v| v.id == id).cloned())
    }

    /// Force a fetch from upstream and write to both caches. Used by the Tier A
    /// poller and by cold-start.
    pub async fn refresh_now(&self) -> Result<Arc<Vec<Vehicle>>, AppError> {
        let mut fetched = up::fetch_all(&self.client).await?;
        // Tier A owns the metadata; for the live columns, whatever saw the
        // vehicle last wins — usually the previous minute's Tier B poll (or,
        // on a cold start, the table it left behind).
        let now = crate::domain::time::utc_now();
        log_last_fix_age(&fetched, now);
        let prior = self.cache.vehicles_snapshot().vehicles;
        let by_id: std::collections::HashMap<&str, &Vehicle> =
            prior.iter().map(|v| (v.id.as_str(), v)).collect();
        for v in &mut fetched {
            if let Some(p) = by_id.get(v.id.as_str()) {
                v.keep_newer_live_from(p, now);
            }
        }
        self.cache.store_vehicles(fetched.clone());
        if let Err(e) = self.repo.replace_all(&fetched).await {
            tracing::warn!(error = %e, "failed to persist vehicles snapshot");
        }
        Ok(self.cache.vehicles_snapshot().vehicles)
    }

    /// Write the vehicle list's live columns to SQLite — called after every
    /// Tier B poll, once [`CacheLayer::store_live`] has folded the readings
    /// in. Best-effort: a failed write costs restart freshness, not the poll.
    pub async fn persist_live(&self) {
        let list = self.cache.vehicles_snapshot().vehicles;
        if list.is_empty() {
            return;
        }
        if let Err(e) = self.repo.update_live(&list).await {
            tracing::warn!(error = %e, "failed to persist live vehicle state");
        }
    }
}

/// One line per Tier A refresh saying how old the freshest fix is. A healthy
/// fleet with moving trucks shows minutes. Hours means LastLocationDate is
/// being read in the wrong zone (behind ⇒ it is UTC read as local; a
/// negative age — ahead of now — ⇒ local read as UTC): see
/// `ETIT_LAST_LOCATION_TZ`.
fn log_last_fix_age(vehicles: &[Vehicle], now: crate::domain::Timestamp) {
    let newest = vehicles.iter().filter_map(|v| v.last_location_at).max();
    if let Some(t) = newest {
        let age = (now - t).num_seconds();
        if age < -600 {
            tracing::warn!(newest_fix = %t, age_secs = age, "tier A: newest fix is in the future; check ETIT_LAST_LOCATION_TZ");
        } else {
            tracing::info!(newest_fix = %t, age_secs = age, "tier A: newest fix");
        }
    }
}