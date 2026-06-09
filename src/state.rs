//! Shared application state plumbed into every handler.
//!
//! Everything in here is wrapped in `Arc` (or is `Clone`-cheap) so the
//! state can be cloned per-request without copying real data.

use std::sync::Arc;
use std::time::Instant;

use crate::cache::CacheLayer;
use crate::config::Config;
use crate::services::broadcaster::Broadcaster;
use crate::services::history::HistoryService;
use crate::services::live::LiveService;
use crate::services::vehicles::VehiclesService;
use crate::upstream::EtitClient;
use crate::db::ZonesRepo;

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
    pub broadcaster: Arc<Broadcaster>,
    pub started_at: Instant,
}
