//! Trip-audit scan orchestration.
//!
//! Nightly (TRIP_SCAN_TIME, default 04:45 — after the zone scan) plus an
//! on-demand API trigger, mirroring the dead-zone scanner:
//!
//!   1. Load the location catalog and planned trips from apex Postgres.
//!   2. Per vehicle, chronologically: pull ETIT history (cache → SQLite →
//!      upstream), reconstruct the trip with the matcher, enrich each leg
//!      with the OSRM optimal, raise flags, persist.
//!   3. Calibration: cluster recent long stops, upsert pin suggestions.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use chrono::{Days, NaiveDate, TimeZone, Utc};

use crate::db::trip_audit::{NewFlag, NewLeg, NewMatch};
use crate::state::AppState;
use crate::zones::geo::haversine_m;
use crate::zones::PointTS;

use super::calibrate;
use super::matcher::{match_trip, MatchInput, MatcherConfig, StopTS};
use super::pg::LocationCatalog;
use super::{Flag, MatchStatus, PlannedTrip, TripScanSummary};

/// How far back the calibration pass looks for long stops (local days).
const CALIBRATION_WINDOW_DAYS: u64 = 14;
/// A stop must dwell at least this long to count for calibration.
const CALIBRATION_MIN_DWELL_SECS: i64 = 600;
/// Matching may start this long before local midnight of the trip date
/// (trucks sometimes load late the previous evening).
const TRIP_WINDOW_HEAD_START_SECS: i64 = 4 * 3600;
/// Legs shorter than this are never flagged for excess distance — ratio
/// noise on short hops isn't actionable.
const MIN_FLAGGABLE_EXCESS_KM: f64 = 2.0;

pub async fn run_scan(state: AppState, dates: Option<Vec<NaiveDate>>) -> TripScanSummary {
    let mut summary = TripScanSummary {
        run_id: 0,
        status: "running".into(),
        days_scanned: 0,
        trips_scanned: 0,
        trips_matched: 0,
        trips_unmatched: 0,
        flags_raised: 0,
        suggestions_made: 0,
        error: None,
    };

    if state.apex_db.is_none() {
        summary.status = "error".into();
        summary.error = Some("APEX_DATABASE_URL not configured; trip audit disabled".into());
        return summary;
    }

    let run_id = match state.trip_repo.insert_scan_run().await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to insert trip_scan_run");
            summary.status = "error".into();
            summary.error = Some(e.to_string());
            return summary;
        }
    };
    summary.run_id = run_id;

    if let Err(e) = do_run_scan(&state, run_id, dates, &mut summary).await {
        tracing::error!(error = %e, "trip scan failed");
        summary.status = "error".into();
        summary.error = Some(format!("{e:#}"));
    } else {
        summary.status = "success".into();
    }

    if let Err(e) = state.trip_repo.finalize_scan_run(run_id, &summary).await {
        tracing::error!(error = %e, "failed to finalize trip_scan_run");
    }
    summary
}

async fn do_run_scan(
    state: &AppState,
    run_id: i64,
    dates: Option<Vec<NaiveDate>>,
    summary: &mut TripScanSummary,
) -> anyhow::Result<()> {
    let apex = state.apex_db.as_ref().unwrap();
    let tz = state.cfg.timezone;

    let catalog = apex
        .load_locations(state.cfg.terminal_radius_m, state.cfg.dropoff_radius_m)
        .await?;

    // ---- Which days ----------------------------------------------------------
    let today = crate::domain::time::today_local(tz);
    let dates: Vec<NaiveDate> = match dates {
        Some(d) if !d.is_empty() => d,
        _ => (1..=state.cfg.trip_scan_lookback_days.max(1) as u64)
            .filter_map(|i| today.checked_sub_days(Days::new(i)))
            .collect(),
    };
    summary.days_scanned = dates.len() as i64;
    let date_strs: Vec<String> = dates.iter().map(|d| d.format("%Y-%m-%d").to_string()).collect();

    let planned = apex.load_planned_trips(&date_strs, &catalog).await?;
    summary.trips_scanned = planned.len() as i64;
    tracing::info!(days = ?date_strs, trips = planned.len(), "trip scan starting");

    // ---- Group by vehicle, chronological ---------------------------------------
    let mut by_vehicle: HashMap<String, Vec<PlannedTrip>> = HashMap::new();
    for trip in planned {
        by_vehicle.entry(trip.etit_car_id.clone()).or_default().push(trip);
    }

    let known_places = catalog.all_places();
    let terminals: Vec<_> = catalog.terminals.values().cloned().collect();
    let matcher_cfg = MatcherConfig {
        tz,
        night_start: state.cfg.tanker_night_start,
        night_end: state.cfg.tanker_night_end,
        unplanned_stop_secs: state.cfg.trip_unplanned_stop_secs,
        duration_ratio_threshold: state.cfg.trip_flag_duration_ratio,
    };

    for (vehicle_id, mut trips) in by_vehicle {
        trips.sort_by(|a, b| (a.day_local.as_str(), a.parent_trip_id)
            .cmp(&(b.day_local.as_str(), b.parent_trip_id)));

        // Unlinked cars produce unmatched rows without touching history.
        let has_gps = !vehicle_id.is_empty();

        // Vehicle history assembled lazily per needed day.
        let mut day_cache: HashMap<NaiveDate, (Vec<PointTS>, Vec<StopTS>)> = HashMap::new();
        let mut last_end_ts: Option<i64> = None;

        for trip in &trips {
            let Ok(day) = NaiveDate::parse_from_str(&trip.day_local, "%Y-%m-%d") else {
                tracing::warn!(parent_trip_id = trip.parent_trip_id, day = %trip.day_local, "unparseable trip date");
                continue;
            };

            let (points, stops) = if has_gps {
                assemble_history(state, &vehicle_id, day, today, &mut day_cache).await
            } else {
                (Vec::new(), Vec::new())
            };

            let window_start = crate::domain::time::start_of_day_utc(day, tz).timestamp()
                - TRIP_WINDOW_HEAD_START_SECS;
            let not_before = last_end_ts.map_or(window_start, |t| t.max(window_start));

            let input = MatchInput {
                planned: trip,
                points: &points,
                stops: &stops,
                known_places: &known_places,
                terminals: &terminals,
                not_before,
            };
            let mut outcome = match_trip(&input, &matcher_cfg);

            // ---- OSRM enrichment (only for legs we detected) ------------------
            let mut osrm_flags: Vec<(Flag, Option<usize>)> = Vec::new();
            for (leg_pos, leg) in outcome.legs.iter_mut().enumerate() {
                match state
                    .osrm
                    .route(leg.from_coords, leg.to_coords, !leg.night_window)
                    .await
                {
                    Ok(route) => {
                        leg.osrm_km = Some(route.distance_km);
                        leg.osrm_secs = Some(route.duration_secs);

                        // Corridor deviation against the optimal geometry.
                        // Use the 95th percentile of per-point deviations, not
                        // the max: a single GPS glitch shouldn't flag a leg.
                        let track = super::geo::decode_polyline(&route.geometry);
                        let leg_points: Vec<(f64, f64)> =
                            super::geo::decode_polyline(&leg.actual_geometry);
                        let mut devs: Vec<f64> = leg_points
                            .iter()
                            .map(|(la, ln)| super::geo::distance_to_track_m(*la, *ln, &track))
                            .collect();
                        devs.sort_by(|a, b| a.total_cmp(b));
                        let p95 = if devs.is_empty() {
                            0.0
                        } else {
                            devs[((devs.len() - 1) as f64 * 0.95) as usize]
                        };
                        leg.max_deviation_m = Some(p95);
                        leg.osrm_geometry = route.geometry;

                        if route.distance_km > 0.1 {
                            let ratio = leg.actual_km / route.distance_km;
                            if ratio > state.cfg.trip_flag_distance_ratio
                                && leg.actual_km - route.distance_km > MIN_FLAGGABLE_EXCESS_KM
                            {
                                osrm_flags.push((
                                    Flag {
                                        flag_type: "excess_distance",
                                        severity: if ratio > state.cfg.trip_flag_distance_ratio * 1.5
                                        {
                                            "critical"
                                        } else {
                                            "warning"
                                        },
                                        details: serde_json::json!({
                                            "leg_seq": leg.seq,
                                            "actual_km": round2(leg.actual_km),
                                            "osrm_km": round2(route.distance_km),
                                            "ratio": round2(ratio),
                                        }),
                                    },
                                    Some(leg_pos),
                                ));
                            }
                        }
                        // Threshold scales with leg length: on a 500 km haul,
                        // two legitimate highway corridors can sit tens of km
                        // apart — only proportionally large detours matter.
                        let dev_threshold = state
                            .cfg
                            .trip_deviation_m
                            .max(route.distance_km * 1000.0 * 0.08);
                        // An equal-length parallel corridor is not
                        // inefficiency — only flag deviations that also cost
                        // distance.
                        let costs_distance =
                            route.distance_km > 0.1 && leg.actual_km / route.distance_km > 1.05;
                        if p95 > dev_threshold && costs_distance {
                            osrm_flags.push((
                                Flag {
                                    flag_type: "route_deviation",
                                    severity: "warning",
                                    details: serde_json::json!({
                                        "leg_seq": leg.seq,
                                        "deviation_m": p95.round(),
                                        "threshold_m": dev_threshold.round(),
                                    }),
                                },
                                Some(leg_pos),
                            ));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            parent_trip_id = trip.parent_trip_id,
                            leg = leg.seq,
                            error = %e,
                            "OSRM route failed; leg left uncompared"
                        );
                    }
                }
            }

            // ---- Trip totals ---------------------------------------------------
            let legs_with_osrm = outcome.legs.iter().filter(|l| l.osrm_km.is_some()).count();
            let actual_km: f64 = outcome.legs.iter().map(|l| l.actual_km).sum();
            let actual_secs: i64 =
                outcome.legs.iter().map(|l| l.arrive_ts - l.depart_ts).sum();
            let (osrm_km, osrm_secs) = if legs_with_osrm == outcome.legs.len()
                && !outcome.legs.is_empty()
            {
                (
                    Some(outcome.legs.iter().filter_map(|l| l.osrm_km).sum::<f64>()),
                    Some(outcome.legs.iter().filter_map(|l| l.osrm_secs).sum::<i64>()),
                )
            } else {
                (None, None)
            };
            let distance_ratio = match (osrm_km, actual_km) {
                (Some(o), a) if o > 0.1 => Some(a / o),
                _ => None,
            };
            let duration_ratio = match (osrm_secs, actual_secs) {
                (Some(o), a) if o > 0 => Some(a as f64 / o as f64),
                _ => None,
            };
            if let Some(r) = duration_ratio {
                if r > state.cfg.trip_flag_duration_ratio {
                    osrm_flags.push((
                        Flag {
                            flag_type: "excess_duration",
                            severity: "warning",
                            details: serde_json::json!({
                                "actual_secs": actual_secs,
                                "osrm_secs": osrm_secs,
                                "ratio": round2(r),
                            }),
                        },
                        None,
                    ));
                }
            }

            let expected_km: Option<f64> = {
                let known: Vec<f64> =
                    trip.deliveries.iter().filter_map(|d| d.expected_km).collect();
                if known.is_empty() { None } else { Some(known.iter().sum()) }
            };

            // ---- Persist -------------------------------------------------------
            // Matcher flags keep their leg attachment via leg.flag_idx.
            let mut flag_leg_pos: Vec<Option<usize>> = vec![None; outcome.flags.len()];
            for (leg_pos, leg) in outcome.legs.iter().enumerate() {
                for &fi in &leg.flag_idx {
                    if let Some(slot) = flag_leg_pos.get_mut(fi) {
                        *slot = Some(leg_pos);
                    }
                }
            }
            let mut all_flags = outcome.flags.clone();
            for (flag, leg_pos) in osrm_flags {
                all_flags.push(flag);
                flag_leg_pos.push(leg_pos);
            }

            let new_legs: Vec<NewLeg> = outcome
                .legs
                .iter()
                .map(|l| NewLeg {
                    seq: l.seq,
                    leg_type: l.leg_type,
                    from_name: &l.from_name,
                    to_name: &l.to_name,
                    depart_ts: epoch_to_rfc3339(l.depart_ts),
                    arrive_ts: epoch_to_rfc3339(l.arrive_ts),
                    night_window: l.night_window,
                    actual_km: l.actual_km,
                    osrm_km: l.osrm_km,
                    actual_secs: l.arrive_ts - l.depart_ts,
                    osrm_secs: l.osrm_secs,
                    distance_ratio: l.osrm_km.and_then(|o| {
                        if o > 0.1 { Some(l.actual_km / o) } else { None }
                    }),
                    max_deviation_m: l.max_deviation_m,
                    actual_geometry: &l.actual_geometry,
                    osrm_geometry: &l.osrm_geometry,
                })
                .collect();
            let new_flags: Vec<NewFlag> = all_flags
                .iter()
                .map(|f| NewFlag {
                    leg_id: None,
                    flag_type: f.flag_type,
                    severity: f.severity,
                    details_json: f.details.to_string(),
                })
                .collect();

            let has_legs = !outcome.legs.is_empty();
            let new_match = NewMatch {
                scan_run_id: run_id,
                parent_trip_id: trip.parent_trip_id,
                day_local: &trip.day_local,
                company: &trip.company,
                terminal_name: &trip.terminal_name,
                car_no_plate: &trip.car_no_plate,
                vehicle_id: if vehicle_id.is_empty() { None } else { Some(&vehicle_id) },
                driver_name: if trip.driver_name.is_empty() { None } else { Some(&trip.driver_name) },
                status: outcome.status.as_str(),
                unmatched_reason: outcome.unmatched_reason,
                start_ts: outcome.start_ts.map(epoch_to_rfc3339),
                end_ts: outcome.end_ts.map(epoch_to_rfc3339),
                returned_to_terminal: outcome.returned_to_terminal,
                deliveries_expected: outcome.deliveries_expected,
                deliveries_visited: outcome.deliveries_visited,
                actual_km: has_legs.then_some(actual_km),
                osrm_km,
                expected_km,
                actual_secs: has_legs.then_some(actual_secs),
                osrm_secs,
                distance_ratio,
                duration_ratio,
            };

            match state
                .trip_repo
                .store_match(&new_match, &new_legs, &new_flags, &flag_leg_pos)
                .await
            {
                Ok(_) => {
                    summary.flags_raised += all_flags.len() as i64;
                    match outcome.status {
                        MatchStatus::Unmatched => summary.trips_unmatched += 1,
                        _ => summary.trips_matched += 1,
                    }
                }
                Err(e) => {
                    tracing::error!(parent_trip_id = trip.parent_trip_id, error = %e, "failed to store trip match");
                }
            }

            if let Some(end) = outcome.end_ts {
                last_end_ts = Some(end);
            }
        }
    }

    // ---- Calibration -----------------------------------------------------------
    match run_calibration(state, run_id, &catalog, today).await {
        Ok(n) => summary.suggestions_made = n,
        Err(e) => tracing::warn!(error = %e, "calibration pass failed"),
    }

    Ok(())
}

/// Merge history for `day - 1 ..= day + 1` (clamped to today), pulling each
/// day through the history service (memory → SQLite → upstream).
async fn assemble_history(
    state: &AppState,
    vehicle_id: &str,
    day: NaiveDate,
    today: NaiveDate,
    cache: &mut HashMap<NaiveDate, (Vec<PointTS>, Vec<StopTS>)>,
) -> (Vec<PointTS>, Vec<StopTS>) {
    let mut wanted: BTreeSet<NaiveDate> = BTreeSet::new();
    if let Some(d) = day.checked_sub_days(Days::new(1)) {
        wanted.insert(d);
    }
    wanted.insert(day);
    if let Some(d) = day.checked_add_days(Days::new(1)) {
        if d <= today {
            wanted.insert(d);
        }
    }

    let mut points = Vec::new();
    let mut stops = Vec::new();
    for d in wanted {
        if !cache.contains_key(&d) {
            let fetched = match state.history.for_day(vehicle_id, d, false).await {
                Ok(resp) => {
                    let pts = resp
                        .points
                        .iter()
                        .filter_map(|p| {
                            p.timestamp.map(|t| PointTS {
                                ts: t.timestamp(),
                                lat: p.lat,
                                lng: p.lng,
                            })
                        })
                        .collect::<Vec<_>>();
                    let sts = resp
                        .stops
                        .iter()
                        .map(|s| StopTS {
                            from: s.from.timestamp(),
                            to: s.to.timestamp(),
                            lat: s.lat,
                            lng: s.lng,
                            address: s.address.clone(),
                        })
                        .collect::<Vec<_>>();
                    (pts, sts)
                }
                Err(e) => {
                    tracing::warn!(vehicle_id, day = %d, error = %e, "history fetch failed for trip scan");
                    (Vec::new(), Vec::new())
                }
            };
            cache.insert(d, fetched);
        }
        let (p, s) = cache.get(&d).unwrap();
        points.extend(p.iter().cloned());
        stops.extend(s.iter().cloned());
    }
    points.sort_by_key(|p| p.ts);
    points.dedup_by_key(|p| p.ts);
    stops.sort_by_key(|s| s.from);
    (points, stops)
}

/// Cluster recent long stops → pin suggestions for the locations inbox.
async fn run_calibration(
    state: &AppState,
    run_id: i64,
    catalog: &LocationCatalog,
    today: NaiveDate,
) -> anyhow::Result<i64> {
    let since = today
        .checked_sub_days(Days::new(CALIBRATION_WINDOW_DAYS))
        .unwrap_or(today)
        .format("%Y-%m-%d")
        .to_string();

    let rows = state.trip_repo.load_recent_stop_rows(&since).await?;

    // (vehicle, day) → long stops; plus one flat list for pinned matching.
    let mut per_vehicle_day: HashMap<(String, String), Vec<(f64, f64)>> = HashMap::new();
    let mut all_stops: Vec<(f64, f64)> = Vec::new();
    for (vehicle_id, day_local, stops_json) in rows {
        let Ok(stops) = serde_json::from_str::<Vec<crate::domain::history::Stop>>(&stops_json)
        else {
            continue;
        };
        for s in stops {
            if (s.to - s.from).num_seconds() < CALIBRATION_MIN_DWELL_SECS {
                continue;
            }
            if s.lat == 0.0 && s.lng == 0.0 {
                continue;
            }
            all_stops.push((s.lat, s.lng));
            per_vehicle_day
                .entry((vehicle_id.clone(), day_local.clone()))
                .or_default()
                .push((s.lat, s.lng));
        }
    }
    if all_stops.is_empty() {
        return Ok(0);
    }

    let mut made = 0i64;

    // ---- Pinned places: does the stop cluster agree with the pin? -------------
    let places: Vec<(&'static str, &super::Place)> = catalog
        .terminals
        .values()
        .map(|p| ("terminal", p))
        .chain(catalog.dropoffs.values().map(|p| ("dropoff", p)))
        .collect();

    for (kind, place) in &places {
        // Stops near this pin, assigned to it only if it's their nearest place
        // (keeps بدر terminal from absorbing شرق مدينة بدر stops and vice versa).
        let nearby: Vec<(f64, f64)> = all_stops
            .iter()
            .filter(|(la, ln)| {
                let d = haversine_m(*la, *ln, place.lat, place.lng);
                if d > calibrate::PINNED_SEARCH_RADIUS_M {
                    return false;
                }
                places.iter().all(|(_, other)| {
                    haversine_m(*la, *ln, other.lat, other.lng) >= d - 1.0
                        || std::ptr::eq(*other, *place)
                })
            })
            .copied()
            .collect();

        if let Some(s) =
            calibrate::suggest_for_pinned(kind, &place.name, (place.lat, place.lng), &nearby)
        {
            state.trip_repo.upsert_suggestion(&s, run_id).await?;
            made += 1;
        }
    }

    // ---- Unpinned places: suggest a pin from serving trucks' stops ------------
    // Drop-offs come from the trips that delivered to them; terminals from
    // the parent trips that departed them. Confident suggestions for a place
    // with NO pin are auto-applied (pin_source = 'gps_suggested') — an empty
    // pin blocks auditing, and the write is guarded to never move a pin a
    // human has set. Corrections to existing pins always stay human-reviewed.
    if let Some(apex) = state.apex_db.as_ref() {
        let mut unpinned: Vec<(&'static str, String, Vec<(f64, f64)>)> = Vec::new();

        let mut collect = |kind: &'static str,
                           service_days: Vec<(String, String, String)>,
                           out: &mut Vec<(&'static str, String, Vec<(f64, f64)>)>| {
            let mut by_name: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
            for (name, day, vehicle) in service_days {
                if let Some(stops) = per_vehicle_day.get(&(vehicle, day)) {
                    let candidates = stops.iter().filter(|(la, ln)| {
                        // Away from every known geofence — those stops are
                        // already explained.
                        places.iter().all(|(_, p)| {
                            haversine_m(*la, *ln, p.lat, p.lng) > p.radius_m + 300.0
                        })
                    });
                    by_name.entry(name).or_default().extend(candidates.copied());
                }
            }
            for (name, stops) in by_name {
                out.push((kind, name, stops));
            }
        };

        if !catalog.unpinned_dropoffs.is_empty() {
            let days = apex
                .load_dropoff_service_days(&since, &catalog.unpinned_dropoffs)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "loading drop-off service days failed");
                    Vec::new()
                });
            collect("dropoff", days, &mut unpinned);
        }
        if !catalog.unpinned_terminals.is_empty() {
            let days = apex
                .load_terminal_service_days(&since, &catalog.unpinned_terminals)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "loading terminal service days failed");
                    Vec::new()
                });
            collect("terminal", days, &mut unpinned);
        }

        for (kind, name, stops) in unpinned {
            if let Some(s) = calibrate::suggest_for_unpinned(kind, &name, &stops) {
                let auto_applied = s.stop_count >= state.cfg.trip_pin_auto_apply_min_stops
                    && apex
                        .apply_pin_if_unpinned(kind, &name, s.suggested_lat, s.suggested_lng)
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!(error = %e, kind, name = %name, "auto-apply pin failed");
                            false
                        });
                state.trip_repo.upsert_suggestion(&s, run_id).await?;
                if auto_applied {
                    tracing::info!(kind, name = %name, stops = s.stop_count, "auto-applied GPS pin");
                    state.trip_repo.mark_suggestion_auto_applied(kind, &name).await?;
                }
                made += 1;
            }
        }
    }

    Ok(made)
}

pub fn spawn_trip_scanner(state: AppState) {
    if state.apex_db.is_none() {
        tracing::info!("APEX_DATABASE_URL not set; trip-audit scanner not started");
        return;
    }
    tokio::spawn(async move {
        if state.cfg.trip_scan_on_startup {
            let _ = run_scan(state.clone(), None).await;
        }
        loop {
            let sleep = duration_until_next_local(&state.cfg.trip_scan_time, state.cfg.timezone);
            tokio::time::sleep(sleep).await;
            let _ = run_scan(state.clone(), None).await;
        }
    });
}

fn duration_until_next_local(time_str: &str, tz: chrono_tz::Tz) -> Duration {
    let now = Utc::now().with_timezone(&tz);
    let target_time = chrono::NaiveTime::parse_from_str(time_str, "%H:%M").unwrap();
    let mut target = tz
        .from_local_datetime(&now.date_naive().and_time(target_time))
        .earliest()
        .unwrap_or(now);
    if target <= now {
        target = target + chrono::Duration::days(1);
    }
    let diff = target.signed_duration_since(now);
    Duration::from_secs(diff.num_seconds().max(60) as u64)
}

fn epoch_to_rfc3339(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
