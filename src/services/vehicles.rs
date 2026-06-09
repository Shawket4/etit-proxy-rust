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
        let fetched = up::fetch_all(&self.client).await?;
        self.cache.store_vehicles(fetched.clone());
        if let Err(e) = self.repo.replace_all(&fetched).await {
            tracing::warn!(error = %e, "failed to persist vehicles snapshot");
        }
        Ok(self.cache.vehicles_snapshot().vehicles)
    }
}
