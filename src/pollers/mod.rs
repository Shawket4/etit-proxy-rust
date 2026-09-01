//! Background pollers.
//!
//! Three workers, each driven by `tokio::time::interval`:
//!
//! - **Tier A** — refresh vehicle metadata (default every 24h, configurable).
//! - **Tier B** — refresh live status every minute and broadcast.
//! - **Tier C** — every 30 minutes, refresh today + yesterday history for
//!   every known vehicle, with a semaphore-bounded fan-out so we don't
//!   thunder-storm the upstream.
//!
//! Workers never hold any synchronous lock across an await. They go through
//! the same auth-aware client as user requests, so the auth Mutex is the
//! only synchronization point and it's only held during the actual login
//! flow.

mod tier_a;
mod tier_b;
mod tier_c;

use std::sync::Arc;

use crate::config::Config;
use crate::db::HistoryRepo;
use crate::services::broadcaster::Broadcaster;
use crate::services::history::HistoryService;
use crate::services::live::LiveService;
use crate::services::vehicles::VehiclesService;

pub struct Pollers {
    pub cfg: Config,
    pub vehicles: Arc<VehiclesService>,
    pub live: Arc<LiveService>,
    pub history: Arc<HistoryService>,
    pub history_repo: HistoryRepo,
    pub broadcaster: Arc<Broadcaster>,
}

impl Pollers {
    pub fn spawn_all(self: Arc<Self>) {
        {
            let p = self.clone();
            crate::observability::spawn_isolated(async move { tier_a::run(p).await; });
        }
        {
            let p = self.clone();
            crate::observability::spawn_isolated(async move { tier_b::run(p).await; });
        }
        {
            let p = self.clone();
            crate::observability::spawn_isolated(async move { tier_c::run(p).await; });
        }
    }
}
