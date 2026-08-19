//! GPS-derived pin calibration.
//!
//! Long stops cluster tightly at real loading/unloading areas. This module
//! turns those clusters into actionable suggestions for the dashboard
//! locations inbox:
//!
//! - **Pinned** places whose stop cluster sits far from the stored pin get a
//!   "move pin" suggestion (real-world case: تعاون مسطرد is ~1.7 km off).
//! - **Unpinned** drop-offs get a "set pin" suggestion computed from stops
//!   made by trucks that actually served them.

use crate::zones::geo::haversine_m;

/// Grid cell size for clustering, meters. Stops at the same facility land in
/// the same or adjacent cells.
const CELL_M: f64 = 200.0;
/// Only consider stops within this range of the stored pin when validating a
/// pinned location.
pub const PINNED_SEARCH_RADIUS_M: f64 = 3_000.0;
/// Suggest moving a pin only when the cluster centroid is at least this far
/// from the stored pin.
const MIN_OFFSET_M: f64 = 250.0;
/// Minimum cluster size for a "move pin" suggestion.
const MIN_STOPS_PINNED: usize = 10;
/// Minimum cluster size for a "set pin" (unpinned) suggestion.
const MIN_STOPS_UNPINNED: usize = 5;

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub kind: &'static str, // "terminal" | "dropoff"
    pub name: String,
    pub suggested_lat: f64,
    pub suggested_lng: f64,
    pub current_lat: Option<f64>,
    pub current_lng: Option<f64>,
    pub offset_m: Option<f64>,
    pub stop_count: i64,
}

/// Centroid of the densest cluster among `stops` (lat, lng pairs), together
/// with the number of stops in that cluster. Uses a fixed grid: the densest
/// cell plus its 8 neighbors form the cluster.
pub fn densest_cluster(stops: &[(f64, f64)]) -> Option<((f64, f64), usize)> {
    if stops.is_empty() {
        return None;
    }
    let lat0 = stops[0].0;
    let cell_lat = CELL_M / 111_320.0;
    let cell_lng = CELL_M / (111_320.0 * lat0.to_radians().cos().abs().max(1e-6));

    use std::collections::HashMap;
    let mut cells: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, (lat, lng)) in stops.iter().enumerate() {
        let key = ((lat / cell_lat).floor() as i64, (lng / cell_lng).floor() as i64);
        cells.entry(key).or_default().push(i);
    }

    // Densest cell by its own count; cluster = cell + 8 neighbors.
    let (&best_key, _) = cells.iter().max_by_key(|(_, v)| v.len())?;
    let mut members: Vec<usize> = Vec::new();
    for dy in -1..=1 {
        for dx in -1..=1 {
            if let Some(v) = cells.get(&(best_key.0 + dy, best_key.1 + dx)) {
                members.extend_from_slice(v);
            }
        }
    }
    let n = members.len();
    let (mut lat_sum, mut lng_sum) = (0.0, 0.0);
    for &i in &members {
        lat_sum += stops[i].0;
        lng_sum += stops[i].1;
    }
    Some(((lat_sum / n as f64, lng_sum / n as f64), n))
}

/// Validate a pinned place against nearby long stops. `stops` must already
/// be filtered to within [`PINNED_SEARCH_RADIUS_M`] of the pin.
pub fn suggest_for_pinned(
    kind: &'static str,
    name: &str,
    pin: (f64, f64),
    stops: &[(f64, f64)],
) -> Option<Suggestion> {
    let ((clat, clng), n) = densest_cluster(stops)?;
    if n < MIN_STOPS_PINNED {
        return None;
    }
    let offset = haversine_m(pin.0, pin.1, clat, clng);
    if offset < MIN_OFFSET_M {
        return None;
    }
    Some(Suggestion {
        kind,
        name: name.to_string(),
        suggested_lat: clat,
        suggested_lng: clng,
        current_lat: Some(pin.0),
        current_lng: Some(pin.1),
        offset_m: Some(offset),
        stop_count: n as i64,
    })
}

/// Suggest a pin for an unpinned drop-off from candidate stops (long stops
/// during trips that served it, away from every known geofence).
pub fn suggest_for_unpinned(name: &str, stops: &[(f64, f64)]) -> Option<Suggestion> {
    let ((clat, clng), n) = densest_cluster(stops)?;
    if n < MIN_STOPS_UNPINNED {
        return None;
    }
    Some(Suggestion {
        kind: "dropoff",
        name: name.to_string(),
        suggested_lat: clat,
        suggested_lng: clng,
        current_lat: None,
        current_lng: None,
        offset_m: None,
        stop_count: n as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn densest_cluster_picks_the_facility_not_the_noise() {
        let mut stops = Vec::new();
        // 20 stops tightly around the real facility.
        for i in 0..20 {
            stops.push((30.1000 + (i % 5) as f64 * 1e-4, 31.2000 + (i % 4) as f64 * 1e-4));
        }
        // 6 scattered roadside stops ~2km away.
        for i in 0..6 {
            stops.push((30.1180 + i as f64 * 3e-3, 31.2180 + i as f64 * 2e-3));
        }
        let ((lat, lng), n) = densest_cluster(&stops).unwrap();
        assert!(n >= 20, "cluster too small: {n}");
        assert!(haversine_m(lat, lng, 30.1002, 31.2001) < 100.0);
    }

    #[test]
    fn pinned_suggestion_only_when_offset_is_large() {
        let facility = (30.1, 31.2);
        let stops: Vec<(f64, f64)> =
            (0..15).map(|i| (facility.0 + (i % 4) as f64 * 1e-4, facility.1)).collect();

        // Pin right on the cluster → no suggestion.
        assert!(suggest_for_pinned("dropoff", "X", facility, &stops).is_none());

        // Pin 1.5km away → move-pin suggestion.
        let s = suggest_for_pinned("dropoff", "X", (30.1135, 31.2), &stops).unwrap();
        assert!(s.offset_m.unwrap() > 1_000.0);
        assert!(haversine_m(s.suggested_lat, s.suggested_lng, facility.0, facility.1) < 100.0);
    }

    #[test]
    fn unpinned_needs_minimum_support() {
        let stops = vec![(30.1, 31.2), (30.1001, 31.2001), (30.1002, 31.2)];
        assert!(suggest_for_unpinned("new station", &stops).is_none());
        let stops: Vec<(f64, f64)> =
            (0..6).map(|i| (30.1 + (i % 3) as f64 * 1e-4, 31.2)).collect();
        assert!(suggest_for_unpinned("new station", &stops).is_some());
    }
}
