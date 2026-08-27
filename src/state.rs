//! Shared application state plumbed into every handler.
//!
//! Everything in here is wrapped in `Arc` (or is `Clone`-cheap) so the
//! state can be cloned per-request without copying real data.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;

use crate::cache::CacheLayer;
use crate::config::Config;
use crate::services::broadcaster::Broadcaster;
use crate::services::history::HistoryService;
use crate::services::live::LiveService;
use crate::services::vehicles::VehiclesService;
use crate::trips::osrm::OsrmClient;
use crate::trips::pg::ApexDb;
use crate::upstream::EtitClient;
use crate::db::{TripAuditRepo, ZonesRepo};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub client: Arc<EtitClient>,
    pub http: reqwest::Client,
    pub cache: Arc<CacheLayer>,
    pub vehicles: Arc<VehiclesService>,
    pub live: Arc<LiveService>,
    pub history: Arc<HistoryService>,
    pub zones_repo: ZonesRepo,
    pub trip_repo: TripAuditRepo,
    /// Read-only apex Postgres; None ⇒ trip audit disabled.
    pub apex_db: Option<ApexDb>,
    pub osrm: OsrmClient,
    pub broadcaster: Arc<Broadcaster>,
    pub started_at: Instant,
    /// Days queued by the FalconGo registration hook, drained by a single
    /// debounced flusher task. `.1` = a flusher is currently scheduled.
    pub pending_rescan: Arc<Mutex<(BTreeSet<chrono::NaiveDate>, bool)>>,
}
