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

    pub async fn get_trip_summary(
        &self,
        vehicle_id: &str,
        from_raw: &str,
        to_raw: &str,
    ) -> Result<crate::domain::history::TripSummary, AppError> {
        if vehicle_id.is_empty() {
            return Err(AppError::BadRequest("vehicle id required".into()));
        }
        let tz = self.cache.tz;
        let from = parse_etit_local(from_raw, tz)
            .ok_or_else(|| AppError::BadRequest(format!("invalid `from` datetime: {from_raw}")))?;
        let to = parse_etit_local(to_raw, tz)
            .ok_or_else(|| AppError::BadRequest(format!("invalid `to` datetime: {to_raw}")))?;

        up::fetch_trip_summary(&self.client, vehicle_id, from, to).await
    }
}

fn internal(e: anyhow::Error) -> AppError {
    AppError::Internal(e)
}
