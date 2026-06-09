//! In-memory caches.
//!
//! - **Vehicles** — a single `ArcSwap<VehiclesSnapshot>`: lock-free reads,
//!   atomic full-replacement writes. Refreshed by Tier A poller.
//! - **Live** — `ArcSwap<LiveSnapshot>`: same shape, fully overwritten
//!   every Tier B tick.
//! - **History (hot/cold)** — a single moka `Cache` keyed by
//!   `(vehicle_id, NaiveDate)`. Hot entries (today/yesterday) and cold ones
//!   live in the same map; the consumer cares only "do I have it". The
//!   TTL is set to a sentinel huge value because we expire by *policy*,
//!   not by elapsed time — Tier C poller overwrites hot rows, and there
//!   is no automatic eviction for cold rows.
//!
//! Design note: the four-tier idea from v1 is preserved, but the
//! mechanical separation between hot/cold collections is gone. With moka
//! you don't need it — capacity-based eviction handles cold growth, and
//! Tier C just rewrites today/yesterday on every tick.

use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use chrono::NaiveDate;
use chrono_tz::Tz;
use moka::future::Cache;
use serde::Serialize;

use crate::domain::history::CachedHistory;
use crate::domain::{LiveVehicleStatus, Vehicle};

#[derive(Clone, Default, Debug)]
pub struct VehiclesSnapshot {
    pub vehicles: Arc<Vec<Vehicle>>,
    pub fetched_at: Option<Instant>,
}

#[derive(Clone, Default, Debug)]
pub struct LiveSnapshot {
    pub by_id: Arc<std::collections::HashMap<String, LiveVehicleStatus>>,
    pub fetched_at: Option<Instant>,
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub struct HistoryKey {
    pub vehicle_id: String,
    pub date: NaiveDate,
}

pub struct CacheLayer {
    vehicles: ArcSwap<VehiclesSnapshot>,
    vehicles_ttl: Duration,

    live: ArcSwap<LiveSnapshot>,

    history: Cache<HistoryKey, Arc<CachedHistory>>,

    pub tz: Tz,
}

impl CacheLayer {
    pub fn new(vehicles_ttl: Duration, tz: Tz, history_capacity: u64) -> Arc<Self> {
        let history = Cache::builder()
            .max_capacity(history_capacity)
            // No TTL: Tier C poller overwrites hot rows; cold rows are
            // capacity-evicted in LRU order under pressure.
            .build();

        Arc::new(Self {
            vehicles: ArcSwap::new(Arc::new(VehiclesSnapshot::default())),
            vehicles_ttl,
            live: ArcSwap::new(Arc::new(LiveSnapshot::default())),
            history,
            tz,
        })
    }

    // ---- Vehicles -----------------------------------------------------------

    pub fn vehicles_snapshot(&self) -> VehiclesSnapshot {
        (**self.vehicles.load()).clone()
    }

    pub fn vehicles_is_fresh(&self) -> bool {
        match self.vehicles.load().fetched_at {
            Some(t) => t.elapsed() < self.vehicles_ttl,
            None => false,
        }
    }

    pub fn store_vehicles(&self, vehicles: Vec<Vehicle>) {
        self.vehicles.store(Arc::new(VehiclesSnapshot {
            vehicles: Arc::new(vehicles),
            fetched_at: Some(Instant::now()),
        }));
    }

    // ---- Live ---------------------------------------------------------------

    pub fn live_snapshot(&self) -> LiveSnapshot {
        (**self.live.load()).clone()
    }

    /// Replace the live snapshot. Plate join is done here using the current
    /// vehicles cache; callers don't need to pre-enrich.
    pub fn store_live(&self, statuses: Vec<LiveVehicleStatus>) {
        let plates: std::collections::HashMap<String, String> = self
            .vehicles_snapshot()
            .vehicles
            .iter()
            .map(|v| (v.id.clone(), v.plate.clone()))
            .collect();

        let map: std::collections::HashMap<String, LiveVehicleStatus> = statuses
            .into_iter()
            .map(|mut s| {
                if s.plate.is_none() {
                    if let Some(p) = plates.get(&s.id) {
                        if !p.is_empty() {
                            s.plate = Some(p.clone());
                        }
                    }
                }
                (s.id.clone(), s)
            })
            .collect();

        self.live.store(Arc::new(LiveSnapshot {
            by_id: Arc::new(map),
            fetched_at: Some(Instant::now()),
        }));
    }

    // ---- History -----------------------------------------------------------

    pub fn today_local(&self) -> NaiveDate {
        crate::domain::time::today_local(self.tz)
    }

    pub fn is_hot_date(&self, date: NaiveDate) -> bool {
        let today = self.today_local();
        let yesterday = today.pred_opt().unwrap_or(today);
        date == today || date == yesterday
    }

    pub async fn get_history(
        &self,
        vehicle_id: &str,
        date: NaiveDate,
    ) -> Option<Arc<CachedHistory>> {
        let key = HistoryKey {
            vehicle_id: vehicle_id.to_string(),
            date,
        };
        self.history.get(&key).await
    }

    pub async fn store_history(
        &self,
        vehicle_id: &str,
        date: NaiveDate,
        history: Arc<CachedHistory>,
    ) {
        let key = HistoryKey {
            vehicle_id: vehicle_id.to_string(),
            date,
        };
        self.history.insert(key, history).await;
    }

    pub async fn invalidate_all(&self) {
        self.vehicles.store(Arc::new(VehiclesSnapshot::default()));
        self.live.store(Arc::new(LiveSnapshot::default()));
        self.history.invalidate_all();
    }

    pub fn stats(&self) -> CacheStats {
        let v = self.vehicles.load();
        let l = self.live.load();
        CacheStats {
            vehicles_count:    v.vehicles.len(),
            vehicles_age_secs: v.fetched_at.map(|t| t.elapsed().as_secs()),
            live_count:        l.by_id.len(),
            live_age_secs:     l.fetched_at.map(|t| t.elapsed().as_secs()),
            history_entries:   self.history.entry_count(),
        }
    }
}

#[derive(Serialize)]
pub struct CacheStats {
    pub vehicles_count: usize,
    pub vehicles_age_secs: Option<u64>,
    pub live_count: usize,
    pub live_age_secs: Option<u64>,
    pub history_entries: u64,
}
