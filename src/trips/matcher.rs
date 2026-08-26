//! Geofence-based trip reconstruction.
//!
//! Pure and synchronous: given GPS points/stops and a planned trip, detect
//! the terminal departure, the drop-off visit sequence, and the (optional)
//! return leg. OSRM comparison happens afterwards in `scan.rs` — this module
//! never does I/O, which keeps it unit-testable.
//!
//! Key behaviors:
//! - Drop-off order is NOT assumed: deliveries are sequenced by when the
//!   truck actually arrived at each geofence.
//! - A return to *any* known terminal counts as the return leg (returning to
//!   the departure terminal is not guaranteed).
//! - Deliveries whose geofence is never visited are flagged, not fatal
//!   (status becomes `partial`).
//! - Stops ≥ threshold outside every known geofence are flagged as
//!   unplanned.

use chrono::TimeZone;
use chrono_tz::Tz;

use crate::zones::geo::{bbox_deltas, haversine_m};
use crate::zones::{PointTS, Visit};

use super::{Flag, MatchOutcome, MatchStatus, Place, PlannedTrip};

/// GPS jumps beyond this are tracker glitches; excluded from leg mileage.
const MAX_JUMP_M: f64 = 5_000.0;
/// A visit only ends after the truck has been outside the fence this long.
const VISIT_GAP_SECS: i64 = 300;
/// Minimum dwell for a geofence hit to count as a real visit. Driving
/// through a station's fence on the way somewhere else takes seconds;
/// loading/unloading takes many minutes.
const MIN_VISIT_DWELL_SECS: i64 = 300;
/// Margin added to a place's radius when deciding whether a long stop was
/// "at" a known location (drivers park at the fence edge).
const STOP_EXEMPT_MARGIN_M: f64 = 150.0;

pub struct MatcherConfig {
    pub tz: Tz,
    pub night_start: chrono::NaiveTime,
    pub night_end: chrono::NaiveTime,
    pub unplanned_stop_secs: i64,
    pub duration_ratio_threshold: f64,
    /// Give up waiting for a terminal return after this long (default 72h).
    pub return_cap_secs: i64,
    /// "Now" as epoch seconds — decides return_pending vs no_return_data.
    pub now_ts: i64,
}

/// A long stop extracted from history, in epoch seconds.
#[derive(Debug, Clone)]
pub struct StopTS {
    pub from: i64,
    pub to: i64,
    pub lat: f64,
    pub lng: f64,
    pub address: String,
}

pub struct MatchInput<'a> {
    pub planned: &'a PlannedTrip,
    /// Sorted ascending by ts; spans the trip date ±1 day.
    pub points: &'a [PointTS],
    pub stops: &'a [StopTS],
    /// Every known geofence (all terminals + all pinned drop-offs), for
    /// unplanned-stop exemption.
    pub known_places: &'a [Place],
    /// All terminals with pins, for return-leg detection.
    pub terminals: &'a [Place],
    /// Matching may not use GPS earlier than this (end of the same
    /// vehicle's previous trip, or the trip-date window start).
    pub not_before: i64,
}

pub fn match_trip(input: &MatchInput, cfg: &MatcherConfig) -> MatchOutcome {
    let planned = input.planned;
    let mut outcome = MatchOutcome {
        status: MatchStatus::Unmatched,
        unmatched_reason: None,
        start_ts: None,
        end_ts: None,
        returned_to_terminal: false,
        return_pending: false,
        deliveries_expected: planned.deliveries.len() as i64,
        deliveries_visited: 0,
        legs: Vec::new(),
        flags: Vec::new(),
    };

    if planned.etit_car_id.is_empty() {
        outcome.unmatched_reason = Some("no_etit_car_id");
        return outcome;
    }
    let Some(terminal) = &planned.terminal else {
        outcome.unmatched_reason = Some("no_terminal_coords");
        return outcome;
    };
    let placed: Vec<_> = planned
        .deliveries
        .iter()
        .filter(|d| d.place.is_some())
        .collect();
    if placed.is_empty() {
        outcome.unmatched_reason = Some("no_dropoff_coords");
        return outcome;
    }
    if input.points.is_empty() {
        outcome.unmatched_reason = Some("no_gps_data");
        return outcome;
    }

    // ---- Detect visit intervals per geofence --------------------------------
    let terminal_visits = detect_visits(input.points, terminal, input.not_before);

    // (delivery index, visits)
    let mut delivery_visits: Vec<(usize, Vec<Visit>)> = Vec::new();
    for (idx, d) in planned.deliveries.iter().enumerate() {
        if let Some(place) = &d.place {
            let visits = detect_visits(input.points, place, input.not_before);
            delivery_visits.push((idx, visits));
        }
    }

    let first_arrival = delivery_visits
        .iter()
        .filter_map(|(_, vs)| vs.first().map(|v| v.entry))
        .min();
    let Some(first_arrival) = first_arrival else {
        outcome.unmatched_reason = Some("no_dropoff_visits");
        return outcome;
    };

    // ---- Terminal departure: latest terminal exit before the first arrival --
    let departure = terminal_visits
        .iter()
        .filter(|v| v.exit < first_arrival)
        .map(|v| v.exit)
        .max();

    // ---- Sequence deliveries by actual arrival, chained chronologically -----
    // cursor = last event we consumed; each next delivery is the one whose
    // earliest visit after the cursor comes first.
    let mut cursor = departure.unwrap_or(input.not_before);
    let mut remaining: Vec<(usize, &Vec<Visit>)> =
        delivery_visits.iter().map(|(i, v)| (*i, v)).collect();
    // (delivery index, visit)
    let mut sequence: Vec<(usize, Visit)> = Vec::new();

    while !remaining.is_empty() {
        let mut best: Option<(usize, usize, Visit)> = None; // (pos in remaining, idx, visit)
        for (pos, (idx, visits)) in remaining.iter().enumerate() {
            if let Some(v) = visits.iter().find(|v| v.entry >= cursor) {
                if best.as_ref().map_or(true, |(_, _, b)| v.entry < b.entry) {
                    best = Some((pos, *idx, v.clone()));
                }
            }
        }
        match best {
            Some((pos, idx, visit)) => {
                cursor = visit.exit.max(visit.entry);
                sequence.push((idx, visit));
                remaining.remove(pos);
            }
            None => break, // the rest were never visited after the cursor
        }
    }

    outcome.deliveries_visited = sequence.len() as i64;

    // Deliveries never visited (either no geofence hit, or unpinned).
    for (idx, d) in planned.deliveries.iter().enumerate() {
        let visited = sequence.iter().any(|(i, _)| *i == idx);
        if !visited {
            let reason = if d.place.is_none() { "no_pin" } else { "geofence_never_entered" };
            outcome.flags.push(Flag {
                flag_type: "skipped_delivery",
                severity: "warning",
                details: serde_json::json!({
                    "drop_off_point": d.drop_off_point,
                    "trip_id": d.trip_id,
                    "reason": reason,
                }),
            });
        }
    }

    if sequence.is_empty() {
        outcome.unmatched_reason = Some("no_dropoff_visits");
        return outcome;
    }

    // ---- Build legs ----------------------------------------------------------
    let mut legs = Vec::new();
    let mut seq_no = 0i64;

    let delivery_place = |idx: usize| -> &Place {
        planned.deliveries[idx]
            .place
            .as_ref()
            .expect("sequenced deliveries always have a place")
    };

    if let Some(dep) = departure {
        let (idx, first) = &sequence[0];
        let to = delivery_place(*idx);
        legs.push(build_leg(
            input.points,
            seq_no,
            "outbound",
            (&terminal.name, terminal.lat, terminal.lng),
            (&to.name, to.lat, to.lng),
            dep,
            first.entry,
            cfg,
        ));
        seq_no += 1;
    } else {
        outcome.flags.push(Flag {
            flag_type: "no_terminal_departure",
            severity: "info",
            details: serde_json::json!({
                "terminal": terminal.name,
                "detail": "no terminal exit found before the first drop-off arrival; outbound leg not scored",
            }),
        });
    }

    for pair in sequence.windows(2) {
        let (from_idx, from_visit) = &pair[0];
        let (to_idx, to_visit) = &pair[1];
        let (from, to) = (delivery_place(*from_idx), delivery_place(*to_idx));
        legs.push(build_leg(
            input.points,
            seq_no,
            "between",
            (&from.name, from.lat, from.lng),
            (&to.name, to.lat, to.lng),
            from_visit.exit,
            to_visit.entry,
            cfg,
        ));
        seq_no += 1;
    }

    // ---- Return leg: first entry into ANY terminal after the last drop ------
    // The natural end of the return is the next terminal entry (the next
    // trip starts by loading at one), capped at `return_cap_secs`. When no
    // entry is found the trip is *open* until the cap passes: nightly scans
    // re-audit pending days, so a Qena run that returns 30h later gets its
    // return leg on a later pass.
    let (last_idx, last_visit) = sequence.last().unwrap();
    let mut trip_end = last_visit.exit;
    let mut best_return: Option<(&Place, i64)> = None;
    for t in input.terminals {
        for v in detect_visits(input.points, t, last_visit.exit) {
            if v.entry > last_visit.exit && v.entry - last_visit.exit <= cfg.return_cap_secs {
                if best_return.map_or(true, |(_, e)| v.entry < e) {
                    best_return = Some((t, v.entry));
                }
            }
        }
    }
    if let Some((ret_terminal, ret_entry)) = best_return {
        let from = delivery_place(*last_idx);
        legs.push(build_leg(
            input.points,
            seq_no,
            "return",
            (&from.name, from.lat, from.lng),
            (&ret_terminal.name, ret_terminal.lat, ret_terminal.lng),
            last_visit.exit,
            ret_entry,
            cfg,
        ));
        outcome.returned_to_terminal = true;
        trip_end = ret_entry;
    } else if cfg.now_ts - last_visit.exit < cfg.return_cap_secs {
        // Still inside the cap — the truck may simply not be back yet.
        outcome.return_pending = true;
        outcome.flags.push(Flag {
            flag_type: "return_pending",
            severity: "info",
            details: serde_json::json!({
                "detail": "trip still open — no terminal entry yet; will re-audit",
                "cap_hours": cfg.return_cap_secs / 3600,
            }),
        });
    } else {
        outcome.flags.push(Flag {
            flag_type: "no_return_data",
            severity: "info",
            details: serde_json::json!({
                "detail": "no terminal entry within the return cap after the last delivery",
                "cap_hours": cfg.return_cap_secs / 3600,
            }),
        });
    }

    let trip_start = departure.unwrap_or(sequence[0].1.entry);
    outcome.start_ts = Some(trip_start);
    outcome.end_ts = Some(trip_end);

    // ---- Unplanned stops -----------------------------------------------------
    for stop in input.stops {
        if stop.to <= trip_start || stop.from >= trip_end {
            continue;
        }
        if stop.to - stop.from < cfg.unplanned_stop_secs {
            continue;
        }
        let at_known_place = input.known_places.iter().any(|p| {
            haversine_m(stop.lat, stop.lng, p.lat, p.lng) <= p.radius_m + STOP_EXEMPT_MARGIN_M
        });
        if at_known_place {
            continue;
        }
        let flag_idx = outcome.flags.len();
        outcome.flags.push(Flag {
            flag_type: "unplanned_stop",
            severity: "warning",
            details: serde_json::json!({
                "lat": stop.lat,
                "lng": stop.lng,
                "from_ts": stop.from,
                "to_ts": stop.to,
                "duration_secs": stop.to - stop.from,
                "address": stop.address,
            }),
        });
        // Attach to the leg whose time range contains the stop.
        if let Some(leg) = legs
            .iter_mut()
            .find(|l| stop.from >= l.depart_ts && stop.from < l.arrive_ts)
        {
            leg.flag_idx.push(flag_idx);
        }
    }

    outcome.legs = legs;
    outcome.status = if outcome.deliveries_visited == outcome.deliveries_expected
        && departure.is_some()
    {
        MatchStatus::Matched
    } else {
        MatchStatus::Partial
    };
    outcome
}

#[allow(clippy::too_many_arguments)]
fn build_leg(
    points: &[PointTS],
    seq: i64,
    leg_type: &'static str,
    from: (&str, f64, f64),
    to: (&str, f64, f64),
    depart_ts: i64,
    arrive_ts: i64,
    cfg: &MatcherConfig,
) -> super::MatchedLeg {
    let raw_points: Vec<(f64, f64, i64)> = points
        .iter()
        .filter(|p| p.ts >= depart_ts && p.ts <= arrive_ts)
        .map(|p| (p.lat, p.lng, p.ts))
        .collect();
    let leg_points: Vec<(f64, f64)> = raw_points.iter().map(|&(la, ln, _)| (la, ln)).collect();

    let actual_km = super::geo::track_length_km(&leg_points, MAX_JUMP_M);
    let actual_geometry = encode_track(&leg_points);

    super::MatchedLeg {
        seq,
        leg_type,
        from_name: from.0.to_string(),
        to_name: to.0.to_string(),
        from_coords: (from.1, from.2),
        to_coords: (to.1, to.2),
        depart_ts,
        arrive_ts,
        night_window: in_night_window(depart_ts, cfg),
        actual_km,
        osrm_km: None,
        osrm_secs: None,
        max_deviation_m: None,
        actual_geometry,
        osrm_geometry: String::new(),
        flag_idx: Vec::new(),
        raw_points,
        distance_method: "chord",
        off_route_pct: None,
    }
}

/// Is this epoch second inside the tanker permit window (local time)?
/// Handles windows that wrap midnight (e.g. 20:00 → 07:00).
fn in_night_window(ts: i64, cfg: &MatcherConfig) -> bool {
    let Some(dt) = chrono::DateTime::from_timestamp(ts, 0) else {
        return false;
    };
    let local = cfg.tz.from_utc_datetime(&dt.naive_utc()).time();
    if cfg.night_start <= cfg.night_end {
        local >= cfg.night_start && local < cfg.night_end
    } else {
        local >= cfg.night_start || local < cfg.night_end
    }
}

/// Visit detection against an arbitrary place (same algorithm as the
/// dead-zone scanner), ignoring points before `not_before`.
fn detect_visits(points: &[PointTS], place: &Place, not_before: i64) -> Vec<Visit> {
    let (dlat, dlng) = bbox_deltas(place.lat, place.radius_m);
    let mut visits = Vec::new();
    let (mut in_visit, mut start, mut last_inside, mut count) = (false, 0i64, 0i64, 0u32);
    for p in points {
        if p.ts < not_before {
            continue;
        }
        let inside = (p.lat - place.lat).abs() <= dlat
            && (p.lng - place.lng).abs() <= dlng
            && haversine_m(p.lat, p.lng, place.lat, place.lng) <= place.radius_m;

        if inside {
            if !in_visit {
                in_visit = true;
                start = p.ts;
                count = 0;
            }
            last_inside = p.ts;
            count += 1;
        } else if in_visit && (p.ts - last_inside) > VISIT_GAP_SECS {
            visits.push(Visit { entry: start, exit: last_inside, points: count });
            in_visit = false;
        }
    }
    if in_visit {
        visits.push(Visit { entry: start, exit: last_inside, points: count });
    }
    visits.retain(|v| v.exit - v.entry >= MIN_VISIT_DWELL_SECS);
    visits
}

pub(crate) fn encode_track(points: &[(f64, f64)]) -> String {
    use crate::domain::history::encode_polyline;
    use crate::domain::HistoryPoint;
    let hp: Vec<HistoryPoint> = points
        .iter()
        .map(|(lat, lng)| HistoryPoint {
            lat: *lat,
            lng: *lng,
            timestamp: None,
            speed: 0,
            speed_limit: 0,
        })
        .collect();
    encode_polyline(&hp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trips::{PlannedDelivery, PlannedTrip};

    fn cfg() -> MatcherConfig {
        MatcherConfig {
            tz: chrono_tz::Africa::Cairo,
            night_start: chrono::NaiveTime::from_hms_opt(20, 0, 0).unwrap(),
            night_end: chrono::NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            unplanned_stop_secs: 900,
            duration_ratio_threshold: 1.5,
        }
    }

    fn place(name: &str, lat: f64, lng: f64, r: f64) -> Place {
        Place { name: name.into(), lat, lng, radius_m: r }
    }

    /// Straight-line points from A to B over [t0, t1].
    fn drive(from: (f64, f64), to: (f64, f64), t0: i64, t1: i64, n: usize) -> Vec<PointTS> {
        (0..=n)
            .map(|i| {
                let f = i as f64 / n as f64;
                PointTS {
                    ts: t0 + ((t1 - t0) as f64 * f) as i64,
                    lat: from.0 + (to.0 - from.0) * f,
                    lng: from.1 + (to.1 - from.1) * f,
                }
            })
            .collect()
    }

    fn planned(terminal: Option<Place>, drops: Vec<(&str, Option<Place>)>) -> PlannedTrip {
        PlannedTrip {
            parent_trip_id: 1,
            day_local: "2026-08-18".into(),
            company: "Watanya".into(),
            terminal_name: "T".into(),
            terminal,
            car_no_plate: "TEST".into(),
            driver_name: "".into(),
            etit_car_id: "guid".into(),
            deliveries: drops
                .into_iter()
                .enumerate()
                .map(|(i, (name, place))| PlannedDelivery {
                    trip_id: i as i64,
                    drop_off_point: name.into(),
                    place,
                    expected_km: None,
                })
                .collect(),
        }
    }

    // Terminal at (30.0, 31.0); drop A ~22km east; drop B ~44km east.
    const TERM: (f64, f64) = (30.0, 31.0);
    const DROP_A: (f64, f64) = (30.0, 31.23);
    const DROP_B: (f64, f64) = (30.0, 31.46);

    fn simple_journey() -> Vec<PointTS> {
        let mut pts = Vec::new();
        // Parked at terminal 08:00–09:00 (epoch base 1_755_500_000 ≈ some day).
        let t = 1_755_500_000i64;
        for i in 0..12 {
            pts.push(PointTS { ts: t + i * 300, lat: TERM.0, lng: TERM.1 });
        }
        // Drive to A over 40 min.
        pts.extend(drive(TERM, DROP_A, t + 3600, t + 6000, 40));
        // Dwell at A for 30 min.
        for i in 0..6 {
            pts.push(PointTS { ts: t + 6000 + i * 300, lat: DROP_A.0, lng: DROP_A.1 });
        }
        // Drive A → B over 40 min.
        pts.extend(drive(DROP_A, DROP_B, t + 7800, t + 10200, 40));
        // Dwell at B.
        for i in 0..6 {
            pts.push(PointTS { ts: t + 10200 + i * 300, lat: DROP_B.0, lng: DROP_B.1 });
        }
        // Return B → terminal.
        pts.extend(drive(DROP_B, TERM, t + 12000, t + 16800, 60));
        for i in 0..3 {
            pts.push(PointTS { ts: t + 16800 + i * 300, lat: TERM.0, lng: TERM.1 });
        }
        pts.sort_by_key(|p| p.ts);
        pts.dedup_by_key(|p| p.ts);
        pts
    }

    #[test]
    fn matches_two_drop_round_trip() {
        let term = place("T", TERM.0, TERM.1, 500.0);
        let a = place("A", DROP_A.0, DROP_A.1, 300.0);
        let b = place("B", DROP_B.0, DROP_B.1, 300.0);
        let p = planned(
            Some(term.clone()),
            vec![("A", Some(a.clone())), ("B", Some(b.clone()))],
        );
        let points = simple_journey();
        let input = MatchInput {
            planned: &p,
            points: &points,
            stops: &[],
            known_places: &[term.clone(), a, b],
            terminals: &[term],
            not_before: 0,
        };
        let out = match_trip(&input, &cfg());
        assert_eq!(out.status, MatchStatus::Matched);
        assert_eq!(out.deliveries_visited, 2);
        assert!(out.returned_to_terminal);
        assert_eq!(out.legs.len(), 3); // outbound, between, return
        assert_eq!(out.legs[0].leg_type, "outbound");
        assert_eq!(out.legs[0].to_name, "A");
        assert_eq!(out.legs[1].to_name, "B");
        assert_eq!(out.legs[2].leg_type, "return");
        // ~22km per hop.
        assert!((out.legs[0].actual_km - 22.2).abs() < 1.5, "got {}", out.legs[0].actual_km);
    }

    #[test]
    fn order_is_detected_not_assumed() {
        // Planned order says A then B, but the truck goes B first.
        let term = place("T", TERM.0, TERM.1, 500.0);
        let a = place("A", DROP_A.0, DROP_A.1, 300.0);
        let b = place("B", DROP_B.0, DROP_B.1, 300.0);
        let t = 1_755_500_000i64;
        let mut pts = Vec::new();
        for i in 0..4 {
            pts.push(PointTS { ts: t + i * 300, lat: TERM.0, lng: TERM.1 });
        }
        pts.extend(drive(TERM, DROP_B, t + 1200, t + 6000, 60));
        for i in 0..4 {
            pts.push(PointTS { ts: t + 6000 + i * 300, lat: DROP_B.0, lng: DROP_B.1 });
        }
        pts.extend(drive(DROP_B, DROP_A, t + 7200, t + 9600, 40));
        for i in 0..4 {
            pts.push(PointTS { ts: t + 9600 + i * 300, lat: DROP_A.0, lng: DROP_A.1 });
        }
        let p = planned(
            Some(term.clone()),
            vec![("A", Some(a.clone())), ("B", Some(b.clone()))],
        );
        let input = MatchInput {
            planned: &p,
            points: &pts,
            stops: &[],
            known_places: &[term.clone(), a, b],
            terminals: &[term],
            not_before: 0,
        };
        let out = match_trip(&input, &cfg());
        assert_eq!(out.legs[0].to_name, "B");
        assert_eq!(out.legs[1].from_name, "B");
        assert_eq!(out.legs[1].to_name, "A");
        // No return in this data.
        assert!(!out.returned_to_terminal);
        assert!(out.flags.iter().any(|f| f.flag_type == "no_return_data"));
    }

    #[test]
    fn missing_car_link_is_unmatched() {
        let term = place("T", TERM.0, TERM.1, 500.0);
        let mut p = planned(Some(term), vec![("A", None)]);
        p.etit_car_id = String::new();
        let input = MatchInput {
            planned: &p,
            points: &[],
            stops: &[],
            known_places: &[],
            terminals: &[],
            not_before: 0,
        };
        let out = match_trip(&input, &cfg());
        assert_eq!(out.status, MatchStatus::Unmatched);
        assert_eq!(out.unmatched_reason, Some("no_etit_car_id"));
    }

    #[test]
    fn unvisited_delivery_flags_partial() {
        let term = place("T", TERM.0, TERM.1, 500.0);
        let a = place("A", DROP_A.0, DROP_A.1, 300.0);
        // Second delivery pinned far away, never visited.
        let c = place("C", 29.0, 30.0, 300.0);
        let t = 1_755_500_000i64;
        let mut pts = Vec::new();
        for i in 0..4 {
            pts.push(PointTS { ts: t + i * 300, lat: TERM.0, lng: TERM.1 });
        }
        pts.extend(drive(TERM, DROP_A, t + 1200, t + 4800, 40));
        for i in 0..4 {
            pts.push(PointTS { ts: t + 4800 + i * 300, lat: DROP_A.0, lng: DROP_A.1 });
        }
        let p = planned(
            Some(term.clone()),
            vec![("A", Some(a.clone())), ("C", Some(c.clone()))],
        );
        let input = MatchInput {
            planned: &p,
            points: &pts,
            stops: &[],
            known_places: &[term.clone(), a, c],
            terminals: &[term],
            not_before: 0,
        };
        let out = match_trip(&input, &cfg());
        assert_eq!(out.status, MatchStatus::Partial);
        assert_eq!(out.deliveries_visited, 1);
        assert!(out
            .flags
            .iter()
            .any(|f| f.flag_type == "skipped_delivery"
                && f.details["drop_off_point"] == "C"));
    }

    #[test]
    fn long_stop_off_route_is_flagged_and_attached_to_leg() {
        let term = place("T", TERM.0, TERM.1, 500.0);
        let a = place("A", DROP_A.0, DROP_A.1, 300.0);
        let points = simple_journey();
        let t = 1_755_500_000i64;
        let p = planned(Some(term.clone()), vec![("A", Some(a.clone()))]);
        // 20-minute stop mid-outbound, well off any geofence.
        let stops = vec![StopTS {
            from: t + 4000,
            to: t + 5200,
            lat: 30.02,
            lng: 31.1,
            address: "somewhere".into(),
        }];
        let input = MatchInput {
            planned: &p,
            points: &points,
            stops: &stops,
            known_places: &[term.clone(), a],
            terminals: &[term],
            not_before: 0,
        };
        let out = match_trip(&input, &cfg());
        let idx = out
            .flags
            .iter()
            .position(|f| f.flag_type == "unplanned_stop")
            .expect("unplanned stop flag");
        assert!(out.legs[0].flag_idx.contains(&idx));
    }

    #[test]
    fn night_window_wraps_midnight() {
        let c = cfg();
        // 23:00 Cairo (UTC+3 in summer 2026 → 20:00 UTC).
        let ts = chrono::DateTime::parse_from_rfc3339("2026-08-18T20:00:00Z")
            .unwrap()
            .timestamp();
        assert!(in_night_window(ts, &c));
        // 12:00 Cairo.
        let ts = chrono::DateTime::parse_from_rfc3339("2026-08-18T09:00:00Z")
            .unwrap()
            .timestamp();
        assert!(!in_night_window(ts, &c));
        // 06:00 Cairo (inside 20:00→07:00).
        let ts = chrono::DateTime::parse_from_rfc3339("2026-08-18T03:00:00Z")
            .unwrap()
            .timestamp();
        assert!(in_night_window(ts, &c));
    }
}
