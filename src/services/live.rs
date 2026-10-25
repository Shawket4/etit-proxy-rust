//! Live-status service.
//!
//! Reads are cheap: the Tier B poller fully overwrites the live snapshot
//! every minute, so handlers just clone the in-memory snapshot. Each refresh
//! also updates the live columns of the vehicle list and the `vehicles`
//! table (see `VehiclesService::persist_live`). On cold
//! start (before the first Tier B tick) we fall back to a synchronous fetch
//! so the dashboard isn't blank for the first 60s.

use std::sync::Arc;

use crate::cache::memory::LiveSnapshot;
use crate::cache::CacheLayer;
use crate::domain::LiveVehicleStatus;
use crate::error::AppError;
use crate::services::vehicles::VehiclesService;
use crate::upstream::{live as up, EtitClient};

pub struct LiveService {
    pub client: Arc<EtitClient>,
    pub cache: Arc<CacheLayer>,
    pub vehicles: Arc<VehiclesService>,
}

impl LiveService {
    /// Snapshot of all vehicles, sorted by id for stable React diffing.
    pub async fn list(&self) -> Result<Vec<LiveVehicleStatus>, AppError> {
        let snap = self.cache.live_snapshot();
        if !snap.by_id.is_empty() {
            let mut out: Vec<LiveVehicleStatus> =
                snap.by_id.values().cloned().collect();
            out.sort_by(|a, b| a.id.cmp(&b.id));
            return Ok(out);
        }
        // Cold start.
        self.refresh_now().await?;
        let snap = self.cache.live_snapshot();
        let mut out: Vec<LiveVehicleStatus> = snap.by_id.values().cloned().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Single-vehicle live status, or 404 if missing.
    pub async fn get_by_id(&self, id: &str) -> Result<LiveVehicleStatus, AppError> {
        let snap = self.cache.live_snapshot();
        if let Some(s) = snap.by_id.get(id) {
            return Ok(s.clone());
        }
        // Cold start path.
        self.refresh_now().await?;
        let snap = self.cache.live_snapshot();
        snap.by_id
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("no live status for vehicle `{id}`")))
    }

    /// Force-fetch from upstream and store. Returns the new snapshot for
    /// the caller's convenience (used by the poller to publish on the
    /// broadcaster).
    pub async fn refresh_now(&self) -> Result<LiveSnapshot, AppError> {
        let vehicles = self.vehicles.list().await?;
        if vehicles.is_empty() {
            // No vehicles to query for. Store an empty snapshot so the
            // freshness clock still ticks.
            self.cache.store_live(Vec::new());
            return Ok(self.cache.live_snapshot());
        }
        let rows = up::fetch_live(&self.client, &vehicles).await?;
        // Also folds the readings into the vehicle list's live columns…
        self.cache.store_live(rows);
        // …which then go to SQLite, so the table and a restart see them too.
        self.vehicles.persist_live().await;
        Ok(self.cache.live_snapshot())
    }
}
