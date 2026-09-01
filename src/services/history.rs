//! History service.
//!
//! Read order for a single calendar day:
//!   1. memory cache,
//!   2. SQLite,
//!   3. upstream (collapsed via `SingleFlight` so concurrent identical
//!      requests for the same `(vehicle, date)` produce only one upstream
//!      call).
//!
//! Range queries (arbitrary `from`/`to`) bypass both caches and go straight
//! to upstream. They're rare and rarely repeat, so caching them adds
//! complexity without proportional benefit.

use std::sync::Arc;

use chrono::NaiveDate;

use crate::cache::memory::CacheLayer;
use crate::cache::SingleFlight;
use crate::db::HistoryRepo;
use crate::domain::history::{encode_polyline, CachedHistory, HistoryResponse};
use crate::domain::time::{end_of_day_utc, parse_etit_local, start_of_day_utc, utc_now};
use crate::error::AppError;
use crate::upstream::{history as up, EtitClient};

pub struct HistoryService {
    pub client: Arc<EtitClient>,
    pub cache: Arc<CacheLayer>,
    pub repo: HistoryRepo,
    pub flight: SingleFlight<(String, NaiveDate), CachedHistory>,
}

impl HistoryService {
    pub fn new(client: Arc<EtitClient>, cache: Arc<CacheLayer>, repo: HistoryRepo) -> Self {
        Self {
            client,
            cache,
            repo,
            flight: SingleFlight::new(),
        }
    }

    /// One full calendar day in the configured timezone.
    pub async fn for_day(
        &self,
        vehicle_id: &str,
        date: NaiveDate,
        force_refresh: bool,
    ) -> Result<HistoryResponse, AppError> {
        if vehicle_id.is_empty() {
            return Err(AppError::BadRequest("vehicle id required".into()));
        }
        let tz = self.cache.tz;
        let from = start_of_day_utc(date, tz);
        let to = end_of_day_utc(date, tz);

        let (cached, was_cache_hit) = self.get_or_fetch_day(vehicle_id, date, force_refresh).await?;

        Ok(HistoryResponse::build(
            vehicle_id.to_string(),
            from,
            to,
            cached.points.clone(),
            cached.stops.clone(),
            cached.sensors.clone(),
            was_cache_hit,
            cached.fetched_at,
        ))
    }

    /// Internal helper: fetch a single full day from Memory -> DB -> Upstream.
    /// Returns (data, is_cache_hit).
    async fn get_or_fetch_day(
        &self,
        vehicle_id: &str,
        date: NaiveDate,
        force_refresh: bool,
    ) -> Result<(CachedHistory, bool), AppError> {
        // 1. Memory cache hit.
        if !force_refresh {
            if let Some(cached) = self.cache.get_history(vehicle_id, date).await {
                if !cached.points.is_empty() {
                    tracing::debug!(vehicle_id, %date, "history memory cache hit");
                    return Ok(((*cached).clone(), true));
                }
            }
        }

        // 2. DB hit (warm cold cache from disk).
        if !force_refresh {
            if let Some(cached) = self.repo.get(vehicle_id, date).await.map_err(internal)? {
                if !cached.points.is_empty() {
                    tracing::info!(vehicle_id, %date, "history DB cache hit (promoting to memory)");
                    self.cache
                        .store_history(vehicle_id, date, Arc::new(cached.clone()))
                        .await;
                    return Ok((cached, true));
                }
            }
        }

        tracing::info!(vehicle_id, %date, force = force_refresh, "history cache miss; fetching upstream");

        // 3. Upstream — collapse concurrent identical requests.
        let tz = self.cache.tz;
        let from = start_of_day_utc(date, tz);
        let to = end_of_day_utc(date, tz);
        let key = (vehicle_id.to_string(), date);
        let client = self.client.clone();
        let cache = self.cache.clone();
        let repo = self.repo.clone(); // Clone repo for use in the closure
        let vid = vehicle_id.to_string();
        let is_hot = self.cache.is_hot_date(date);

        let cached: CachedHistory = self
            .flight
            .run(key, || async move {
                let upstream = up::fetch_range(&client, &vid, from, to).await?;

                // --- Safety Latch ---
                // If upstream returns 0 points, check if we already have data.
                // If we do, don't overwrite with an empty set.
                if upstream.points.is_empty() {
                    if let Ok(Some(existing)) = repo.get(&vid, date).await {
                        if !existing.points.is_empty() {
                            tracing::warn!(
                                vehicle_id = %vid,
                                %date,
                                "upstream returned empty history but local DB has data; ignoring wipe"
                            );
                            return Ok(existing);
                        }
                    }
                }

                let geometry = encode_polyline(&upstream.points);
                let fetched_at = utc_now();
                let cached = CachedHistory {
                    points: upstream.points,
                    stops: upstream.stops,
                    sensors: upstream.sensors,
                    geometry,
                    fetched_at,
                };
                cache
                    .store_history(&vid, date, Arc::new(cached.clone()))
                    .await;
                Ok::<_, AppError>(cached)
            })
            .await?;

        // Persist to DB outside the singleflight closure.
        if let Err(e) = self.repo.upsert(vehicle_id, date, &cached, is_hot).await {
            tracing::warn!(vehicle_id, %date, error = %e, "history DB upsert failed");
        }

        Ok((cached, false))
    }

    /// Arbitrary `from`/`to` range. Strings come from the API in either
    /// `YYYY-MM-DDTHH:MM:SS` (interpreted as local time in the configured
    /// timezone) or full RFC3339 with offset.
    pub async fn for_range(
        &self,
        vehicle_id: &str,
        from_raw: &str,
        to_raw: &str,
        force_refresh: bool,
    ) -> Result<HistoryResponse, AppError> {
        if vehicle_id.is_empty() {
            return Err(AppError::BadRequest("vehicle id required".into()));
        }
        let tz = self.cache.tz;
        let from = parse_etit_local(from_raw, tz)
            .ok_or_else(|| AppError::BadRequest(format!("invalid `from` datetime: {from_raw}")))?;
        let to = parse_etit_local(to_raw, tz)
            .ok_or_else(|| AppError::BadRequest(format!("invalid `to` datetime: {to_raw}")))?;
        if to <= from {
            return Err(AppError::BadRequest("`to` must be after `from`".into()));
        }

        // 1. Identify which calendar days this range spans in the local timezone.
        let today = self.cache.today_local();
        let start_date = from.with_timezone(&tz).date_naive();
        let end_date = to.with_timezone(&tz).date_naive().min(today);

        if start_date > today {
             // Requesting entirely in the future? Return early.
             return Ok(HistoryResponse::build(
                 vehicle_id.to_string(),
                 from,
                 to,
                 Vec::new(),
                 Vec::new(),
                 Vec::new(),
                 true,
                 utc_now(),
             ));
        }

        let mut all_points = Vec::new();
        let mut all_stops = Vec::new();
        let mut all_sensors = Vec::new();
        let mut all_cached = true;
        let mut oldest_fetch = utc_now();

        // 2. Fetch each full day and collect the parts.
        let mut curr = start_date;
        while curr <= end_date {
            let (day_data, is_hit) = self.get_or_fetch_day(vehicle_id, curr, force_refresh).await?;
            all_points.extend(day_data.points);
            all_stops.extend(day_data.stops);
            all_sensors.extend(day_data.sensors);
            
            if !is_hit {
                all_cached = false;
            }
            if day_data.fetched_at < oldest_fetch {
                oldest_fetch = day_data.fetched_at;
            }

            curr = curr.succ_opt().unwrap_or(curr);
            if curr == end_date.succ_opt().unwrap_or(end_date) {
                // succ_opt overflow protection
                if curr <= end_date { continue; } else { break; }
            }
        }

        // 3. Filter to the exact window.
        let points: Vec<_> = all_points
            .into_iter()
            .filter(|p| {
                if let Some(t) = p.timestamp {
                    t >= from && t <= to
                } else {
                    false
                }
            })
            .collect();

        let stops: Vec<_> = all_stops
            .into_iter()
            .filter(|s| s.from >= from && s.from <= to)
            .collect();

        let sensors: Vec<_> = all_sensors
            .into_iter()
            .filter(|s| s.timestamp >= from && s.timestamp <= to)
            .collect();

        // 4. Build the final response (geometry is re-encoded for the filtered set).
        Ok(HistoryResponse::build(
            vehicle_id.to_string(),
            from,
            to,
            points,
            stops,
            sensors,
            all_cached,
            oldest_fetch,
        ))
    }

    /// Deterministic summary computed from the points/stops/ignition events
    /// inside the exact requested window — replaces ETIT's asynchronous
    /// `GetHistoryTripSummary` upstream call, whose numbers keep changing
    /// for the same query while ETIT's ingest settles. Nothing here is
    /// persisted: same window in, same numbers out, always derived from the
    /// same cached raw data the map draws.
    ///
    /// Distance is the teleport-filtered chord sum over the raw pings —
    /// deliberately NOT map-matched: measured head-to-head, HMM matching a
    /// full day costs up to ~30s of OSRM CPU on dense urban traces for a
    /// ~0.3% distance difference (422.4 raw vs 423.5 matched km on the
    /// benchmark day), and interactive dashboard traffic doing that starves
    /// the audit's OSRM. The nightly audit keeps road-snapped precision
    /// where batch latency is free.
    pub async fn computed_summary(
        &self,
        vehicle_id: &str,
        from_raw: &str,
        to_raw: &str,
        force_refresh: bool,
    ) -> Result<crate::domain::summary::ComputedSummary, AppError> {
        if vehicle_id.is_empty() {
            return Err(AppError::BadRequest("vehicle id required".into()));
        }
        let tz = self.cache.tz;
        let from = parse_etit_local(from_raw, tz)
            .ok_or_else(|| AppError::BadRequest(format!("invalid `from` datetime: {from_raw}")))?;
        let to = parse_etit_local(to_raw, tz)
            .ok_or_else(|| AppError::BadRequest(format!("invalid `to` datetime: {to_raw}")))?;
        if to <= from {
            return Err(AppError::BadRequest("`to` must be after `from`".into()));
        }

        // Unsimplified parts: the map response compresses the polyline,
        // which is fine for drawing but would distort distance and the
        // moving/stationary split.
        let (points, stops, sensors) =
            self.collect_window(vehicle_id, from, to, force_refresh).await?;

        let (summary, _raw_km) =
            crate::domain::summary::compute_metrics(&points, &stops, &sensors, from, to);
        Ok(summary)
    }

    /// Day-cache-backed fetch of the raw (unsimplified) parts inside an
    /// exact window.
    async fn collect_window(
        &self,
        vehicle_id: &str,
        from: crate::domain::time::Timestamp,
        to: crate::domain::time::Timestamp,
        force_refresh: bool,
    ) -> Result<
        (
            Vec<crate::domain::history::HistoryPoint>,
            Vec<crate::domain::history::Stop>,
            Vec<crate::domain::history::SensorEvent>,
        ),
        AppError,
    > {
        let tz = self.cache.tz;
        let today = self.cache.today_local();
        let start_date = from.with_timezone(&tz).date_naive();
        let end_date = to.with_timezone(&tz).date_naive().min(today);
        if start_date > today {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }

        let mut points = Vec::new();
        let mut stops = Vec::new();
        let mut sensors = Vec::new();
        let mut curr = start_date;
        while curr <= end_date {
            let (day, _) = self.get_or_fetch_day(vehicle_id, curr, force_refresh).await?;
            points.extend(day.points);
            stops.extend(day.stops);
            sensors.extend(day.sensors);
            match curr.succ_opt() {
                Some(n) => curr = n,
                None => break,
            }
        }
        points.retain(|p| p.timestamp.map_or(false, |t| t >= from && t <= to));
        stops.retain(|s| s.from >= from && s.from <= to);
        sensors.retain(|s| s.timestamp >= from && s.timestamp <= to);
        Ok((points, stops, sensors))
    }
}

/// When the nightly healer runs (local time) — before the 04:45 audit scan
/// so healed days are re-audited the same night.
const HEAL_TIME: &str = "03:10";
/// How far back it re-fetches.
const HEAL_DAYS: u64 = 7;

/// Nightly late-data healer: trackers buffering through dead zones upload a
/// day's GPS hours or days late, and ETIT keeps settling recent history —
/// the same instability that made its summary endpoint unreliable. Force-
/// refetch the last N days per vehicle (the safety latch in
/// `get_or_fetch_day` refuses empty overwrites), and queue a trip-audit
/// rescan for any day whose point count changed.
pub fn spawn_history_healer(state: crate::state::AppState) {
    crate::observability::spawn_isolated(async move {
        loop {
            let sleep =
                crate::trips::scan::duration_until_next_local(HEAL_TIME, state.cfg.timezone);
            tokio::time::sleep(sleep).await;
            let st = state.clone();
            // Panic isolation: a bad day must not kill the scheduler loop.
            if let Err(e) = tokio::spawn(async move { heal_recent_history(st).await }).await {
                tracing::error!(error = %e, "history healer panicked");
            }
        }
    });
}

async fn heal_recent_history(state: crate::state::AppState) {
    let vehicles = match state.vehicles.list().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "history healer: vehicle list unavailable");
            return;
        }
    };
    let today = state.cache.today_local();
    let mut changed_days = std::collections::BTreeSet::new();
    let mut refreshed = 0usize;
    for v in vehicles.iter() {
        for back in 1..=HEAL_DAYS {
            let Some(day) = today.checked_sub_days(chrono::Days::new(back)) else {
                continue;
            };
            let before = match state.history.repo.get(&v.id, day).await {
                Ok(r) => r.map(|c| c.points.len()).unwrap_or(0),
                Err(_) => 0,
            };
            match state.history.get_or_fetch_day(&v.id, day, true).await {
                Ok((after, _)) => {
                    refreshed += 1;
                    if after.points.len() != before {
                        tracing::info!(
                            vehicle = %v.id, %day, before, after = after.points.len(),
                            "history healed: point count changed"
                        );
                        changed_days.insert(day);
                    }
                }
                Err(e) => {
                    tracing::debug!(vehicle = %v.id, %day, error = %e, "history heal fetch failed")
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
    }
    if !changed_days.is_empty() {
        let queued =
            crate::trips::scan::queue_rescan_days(&state, changed_days.iter().cloned().collect());
        tracing::info!(queued, "history healer queued audit rescans");
    }
    tracing::info!(refreshed, changed = changed_days.len(), "history heal pass complete");
}

fn internal(e: anyhow::Error) -> AppError {
    AppError::Internal(e)
}
