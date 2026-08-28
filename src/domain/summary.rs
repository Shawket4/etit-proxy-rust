//! Computed trip summary — replaces ETIT's `GetHistoryTripSummary.ashx`.
//!
//! ETIT computes its summary asynchronously from data that is still settling,
//! so the same query returns different numbers for a while before converging.
//! We instead derive every figure deterministically from the GPS points,
//! stops, and ignition events we already serve to the map — same window in,
//! same numbers out, and the panel can never disagree with the drawn track.
//!
//! Distance comes from OSRM map-matching (road-snapped, immune to GPS
//! scatter) with a raw haversine sum as fallback — the same
//! `matched`/`chord` policy the trip audit uses.

use serde::Serialize;

use super::history::{HistoryPoint, SensorEvent, Stop};
use super::time::Timestamp;

/// Ping gaps longer than this count as tracker-disconnected time, and the
/// pair is excluded from the moving/stationary split.
const DISCONNECT_GAP_SECS: i64 = 600;

/// A pair of consecutive pings is "moving" when either fix reports at least
/// this speed, or the implied straight-line speed reaches it.
const MOVING_KMH: f64 = 3.0;

/// Implied speeds above this are GPS teleports; their chord distance is
/// noise, not driving.
const TELEPORT_KMH: f64 = 160.0;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputedSummary {
    /// Kilometres over the requested window. Road-snapped when
    /// `distance_method == "matched"`, chord-sum otherwise.
    pub mileage_km: f64,
    /// "matched" (OSRM map-match) or "raw" (haversine sum fallback).
    pub distance_method: String,
    /// Fraction of pings the matcher accepted, when matched (0..100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_coverage_pct: Option<f64>,
    /// Seconds in motion.
    pub active_secs: i64,
    /// Seconds stationary with ignition on.
    pub idle_secs: i64,
    /// Seconds stationary with ignition off (or unknown).
    pub passive_secs: i64,
    /// Seconds inside ping gaps longer than the disconnect threshold.
    pub disconnected_secs: i64,
    pub stop_count: i64,
    pub ignition_on_count: i64,
    pub ignition_off_count: i64,
    /// Pings inside the window — lets the UI qualify thin data.
    pub point_count: i64,
    pub from: Timestamp,
    pub to: Timestamp,
}

/// Everything except mileage/method, which need OSRM and are filled by the
/// caller. Returns (partial summary, raw chord km) — the raw figure doubles
/// as both the fallback mileage and the sanity band for the matched one.
pub fn compute_metrics(
    points: &[HistoryPoint],
    stops: &[Stop],
    sensors: &[SensorEvent],
    from: Timestamp,
    to: Timestamp,
) -> (ComputedSummary, f64) {
    let mut timed: Vec<(&HistoryPoint, Timestamp)> = points
        .iter()
        .filter_map(|p| p.timestamp.map(|t| (p, t)))
        .collect();
    timed.sort_by_key(|(_, t)| *t);

    let mut raw_km = 0.0;
    let mut active = 0i64;
    let mut disconnected = 0i64;
    // Stationary intervals get their ignition state resolved afterwards.
    let mut stationary: Vec<(Timestamp, i64)> = Vec::new();

    for pair in timed.windows(2) {
        let (p1, t1) = pair[0];
        let (p2, t2) = pair[1];
        let dt = (t2 - t1).num_seconds();
        if dt <= 0 {
            continue;
        }
        let d_m = crate::zones::geo::haversine_m(p1.lat, p1.lng, p2.lat, p2.lng);
        let implied_kmh = d_m / 1000.0 / (dt as f64) * 3600.0;

        if dt > DISCONNECT_GAP_SECS {
            disconnected += dt;
            // The truck may well have driven through the gap; the chord is
            // the honest lower bound (the matcher bridges fragments the
            // same way, so matched and raw stay comparable).
            if implied_kmh <= TELEPORT_KMH {
                raw_km += d_m / 1000.0;
            }
            continue;
        }
        if implied_kmh > TELEPORT_KMH {
            // GPS teleport: ignore the jump, bank the time as stationary
            // (the next sane pair will pick the track back up).
            stationary.push((t1, dt));
            continue;
        }
        raw_km += d_m / 1000.0;
        let moving =
            p1.speed as f64 >= MOVING_KMH || p2.speed as f64 >= MOVING_KMH || implied_kmh >= MOVING_KMH;
        if moving {
            active += dt;
        } else {
            stationary.push((t1, dt));
        }
    }

    // Ignition timeline: events sorted by time; state at any instant is the
    // most recent event at or before it (unknown before the first event).
    let mut ign: Vec<(&SensorEvent, bool)> = sensors
        .iter()
        .filter_map(|s| match s.type_name.as_str() {
            "Ignition On" => Some((s, true)),
            "Ignition Off" => Some((s, false)),
            _ => None,
        })
        .collect();
    ign.sort_by_key(|(s, _)| s.timestamp);

    let state_at = |t: Timestamp| -> Option<bool> {
        match ign.partition_point(|(s, _)| s.timestamp <= t) {
            0 => None,
            i => Some(ign[i - 1].1),
        }
    };

    let mut idle = 0i64;
    let mut passive = 0i64;
    for (start, dt) in &stationary {
        let mid = *start + chrono::Duration::seconds(dt / 2);
        match state_at(mid) {
            Some(true) => idle += dt,
            // Engine off — and unknown defaults to passive, the
            // conservative read for a parked truck.
            _ => passive += dt,
        }
    }

    let on_count = ign.iter().filter(|(_, on)| *on).count() as i64;
    let off_count = ign.len() as i64 - on_count;

    let summary = ComputedSummary {
        mileage_km: round1(raw_km),
        distance_method: "raw".into(),
        match_coverage_pct: None,
        active_secs: active,
        idle_secs: idle,
        passive_secs: passive,
        disconnected_secs: disconnected,
        stop_count: stops.len() as i64,
        ignition_on_count: on_count,
        ignition_off_count: off_count,
        point_count: timed.len() as i64,
        from,
        to,
    };
    (summary, raw_km)
}

pub fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn pt(lat: f64, lng: f64, secs: i64, speed: u32) -> HistoryPoint {
        HistoryPoint {
            lat,
            lng,
            timestamp: Some(Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()),
            speed,
            speed_limit: 0,
        }
    }

    fn ign(secs: i64, on: bool) -> SensorEvent {
        SensorEvent {
            lat: 0.0,
            lng: 0.0,
            timestamp: Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap(),
            type_name: if on { "Ignition On" } else { "Ignition Off" }.into(),
        }
    }

    fn window() -> (Timestamp, Timestamp) {
        (
            Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            Utc.timestamp_opt(1_700_010_000, 0).unwrap(),
        )
    }

    #[test]
    fn moving_time_and_distance_accumulate() {
        // ~1.11 km hops, 60 s apart -> ~67 km/h.
        let pts = vec![
            pt(30.00, 31.00, 0, 60),
            pt(30.01, 31.00, 60, 60),
            pt(30.02, 31.00, 120, 60),
        ];
        let (s, raw) = compute_metrics(&pts, &[], &[], window().0, window().1);
        assert_eq!(s.active_secs, 120);
        assert_eq!(s.idle_secs + s.passive_secs, 0);
        assert!(raw > 2.0 && raw < 2.4, "raw {raw}");
    }

    #[test]
    fn stationary_splits_by_ignition() {
        let pts = vec![
            pt(30.0, 31.0, 0, 0),
            pt(30.0, 31.0, 300, 0),   // idle: ignition turned on at t=10
            pt(30.0, 31.0, 600, 0),   // still on
            pt(30.0, 31.0, 900, 0),   // off at t=650 -> passive
        ];
        let sensors = vec![ign(10, true), ign(650, false)];
        let (s, _) = compute_metrics(&pts, &[], &sensors, window().0, window().1);
        assert_eq!(s.active_secs, 0);
        assert_eq!(s.idle_secs, 600);
        assert_eq!(s.passive_secs, 300);
        assert_eq!(s.ignition_on_count, 1);
        assert_eq!(s.ignition_off_count, 1);
    }

    #[test]
    fn long_gap_counts_disconnected_not_active() {
        let pts = vec![
            pt(30.00, 31.00, 0, 50),
            pt(30.10, 31.00, 3600, 50), // 1 h gap, ~11 km apart
        ];
        let (s, raw) = compute_metrics(&pts, &[], &[], window().0, window().1);
        assert_eq!(s.disconnected_secs, 3600);
        assert_eq!(s.active_secs, 0);
        assert!(raw > 10.0, "gap chord still counted: {raw}");
    }

    #[test]
    fn teleport_distance_is_dropped() {
        let pts = vec![
            pt(30.0, 31.0, 0, 0),
            pt(31.0, 31.0, 60, 0), // ~111 km in 60 s -> teleport
            pt(31.0, 31.0, 120, 0),
        ];
        let (_, raw) = compute_metrics(&pts, &[], &[], window().0, window().1);
        assert!(raw < 0.1, "teleport not filtered: {raw}");
    }
}
