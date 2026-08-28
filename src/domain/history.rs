//! `HistoryPoint`, `HistoryResponse`, and the Google-compatible polyline
//! encoder.
//!
//! The encoder is precision-5 (~1.1 m), matching Google Maps'
//! `geometry.encoding.decodePath`.

use serde::{Deserialize, Serialize};

use super::time::Timestamp;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPoint {
    pub lat: f64,
    pub lng: f64,
    pub timestamp: Option<Timestamp>,
    pub speed: u32,
    pub speed_limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stop {
    pub lat: f64,
    pub lng: f64,
    pub from: Timestamp,
    pub to: Timestamp,
    pub duration: String,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SensorEvent {
    pub lat: f64,
    pub lng: f64,
    pub timestamp: Timestamp,
    pub type_name: String,
}#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripSummary {
    pub total_mileage: String,
    pub total_active_time: String,
    pub total_passive_time: String,
    pub total_idle_time: String,
    pub driver_name: String,
    pub number_of_stops: String,
    pub total_disconnected_time: String,
    pub total_fuel_consumption: String,
    pub ignition_off_count: String,
    pub ignition_on_count: String,
}

/// A place the vehicle meaningfully dwelled at inside the window — fed from
/// the trip-audit's matched legs (terminals, drop-offs, garages) and its
/// unplanned-stop flags, so the live-tracking map pins agree with the audit
/// by construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignificantPin {
    pub name: String,
    /// "terminal" | "dropoff" | "garage" | "stop"
    pub kind: String,
    pub lat: f64,
    pub lng: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arrive: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depart: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dwell_secs: Option<i64>,
    /// The registered trip this pin belongs to; None for unplanned stops.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_trip_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResponse {
    pub vehicle_id: String,
    pub from: Timestamp,
    pub to: Timestamp,
    pub points: Vec<HistoryPoint>,
    pub stops: Vec<Stop>,
    pub sensors: Vec<SensorEvent>,
    pub geometry: String,
    pub cached: bool,
    pub fetched_at: Timestamp,
    /// Audit-derived dwell pins; empty when the audit has no matches here.
    #[serde(default)]
    pub pins: Vec<SignificantPin>,
}

impl HistoryResponse {
    pub fn build(
        vehicle_id: String,
        from: Timestamp,
        to: Timestamp,
        points: Vec<HistoryPoint>,
        stops: Vec<Stop>,
        sensors: Vec<SensorEvent>,
        cached: bool,
        fetched_at: Timestamp,
    ) -> Self {
        // Apply dynamic compression to both the points array and the geometry.
        // We include stops and sensors so their points are never simplified away.
        let points = simplify_points(&points, &stops, &sensors);
        let geometry = encode_polyline(&points);

        Self {
            vehicle_id,
            from,
            to,
            points,
            stops,
            sensors,
            geometry,
            cached,
            fetched_at,
            pins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CachedHistory {
    pub points: Vec<HistoryPoint>,
    pub stops: Vec<Stop>,
    pub sensors: Vec<SensorEvent>,
    pub geometry: String,
    pub fetched_at: Timestamp,
}

/// Encode a polyline at precision 5. Compatible with
/// `google.maps.geometry.encoding.decodePath`.
pub fn encode_polyline(points: &[HistoryPoint]) -> String {
    if points.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(points.len() * 4);
    let mut last_lat = 0i64;
    let mut last_lng = 0i64;

    for p in points {
        let lat = (p.lat * 1e5).round() as i64;
        let lng = (p.lng * 1e5).round() as i64;
        encode_value((lat - last_lat) as i32, &mut out);
        encode_value((lng - last_lng) as i32, &mut out);
        last_lat = lat;
        last_lng = lng;
    }
    out
}

/// Decode a precision-5 polyline. Inverse of [`encode_polyline`]; tolerant
/// of truncated input (returns what it decoded so far).
pub fn decode_polyline(poly: &str) -> Vec<(f64, f64)> {
    let bytes = poly.as_bytes();
    let mut coords = Vec::new();
    let (mut i, mut lat, mut lng) = (0usize, 0i64, 0i64);
    while i < bytes.len() {
        let next = |i: &mut usize| -> Option<i64> {
            let (mut result, mut shift) = (0i64, 0u32);
            loop {
                let b = (*bytes.get(*i)? as i64) - 63;
                *i += 1;
                result |= (b & 0x1f) << shift;
                shift += 5;
                if b < 0x20 {
                    break;
                }
            }
            Some(if result & 1 != 0 { !(result >> 1) } else { result >> 1 })
        };
        let (Some(dlat), Some(dlng)) = (next(&mut i), next(&mut i)) else { break };
        lat += dlat;
        lng += dlng;
        coords.push((lat as f64 / 1e5, lng as f64 / 1e5));
    }
    coords
}

pub fn simplify_points(
    points: &[HistoryPoint],
    stops: &[Stop],
    sensors: &[SensorEvent],
) -> Vec<HistoryPoint> {
    if points.len() <= 2 {
        return points.to_vec();
    }

    use std::collections::HashSet;
    let mut critical_times = HashSet::new();
    for s in stops {
        critical_times.insert(s.from);
        critical_times.insert(s.to);
    }
    for s in sensors {
        if s.type_name.to_lowercase().contains("ignition") {
            critical_times.insert(s.timestamp);
        }
    }

    let mut simplified = Vec::with_capacity(points.len() / 2);
    simplified.push(points[0].clone());

    let mut last_idx = 0;
    for i in 1..points.len() {
        let last = &points[last_idx];
        let curr = &points[i];

        // Always keep the last point.
        if i == points.len() - 1 {
            simplified.push(curr.clone());
            break;
        }

        // --- Preservation check (Stops/Ignitions) ---
        let is_critical = curr
            .timestamp
            .map(|t| critical_times.contains(&t))
            .unwrap_or(false);

        if is_critical {
            simplified.push(curr.clone());
            last_idx = i;
            continue;
        }

        let d2 = distance_meters_sq(last, curr);

        // 1. Radial filter: skip if too close (< 5m).
        if d2 < 25.0 {
            continue;
        }

        // 2. Max segment length: keep if we've gone > 2km on a straight.
        if d2 > 4_000_000.0 {
            simplified.push(curr.clone());
            last_idx = i;
            continue;
        }

        // 3. Perpendicular deviation check.
        let next = &points[i + 1];
        let perp = perpendicular_distance_meters(last, next, curr);

        if perp > 15.0 {
            simplified.push(curr.clone());
            last_idx = i;
        }
    }
    simplified
}

fn distance_meters_sq(p1: &HistoryPoint, p2: &HistoryPoint) -> f64 {
    let lat_mid = (p1.lat + p2.lat) / 2.0;
    let cos_lat = (lat_mid.to_radians()).cos();
    let dy = (p1.lat - p2.lat) * 111_320.0;
    let dx = (p1.lng - p2.lng) * 111_320.0 * cos_lat;
    dy * dy + dx * dx
}

fn perpendicular_distance_meters(a: &HistoryPoint, b: &HistoryPoint, p: &HistoryPoint) -> f64 {
    let lat_mid = (a.lat + b.lat) / 2.0;
    let cos_lat = (lat_mid.to_radians()).cos();
    
    let x1 = a.lng * 111_320.0 * cos_lat;
    let y1 = a.lat * 111_320.0;
    let x2 = b.lng * 111_320.0 * cos_lat;
    let y2 = b.lat * 111_320.0;
    let xp = p.lng * 111_320.0 * cos_lat;
    let yp = p.lat * 111_320.0;

    let num = ((y2 - y1) * xp - (x2 - x1) * yp + x2 * y1 - y2 * x1).abs();
    let den = ((y2 - y1).powi(2) + (x2 - x1).powi(2)).sqrt();
    
    if den < 0.01 {
        return ((xp - x1).powi(2) + (yp - y1).powi(2)).sqrt();
    }
    num / den
}

fn encode_value(v: i32, out: &mut String) {
    let mut v = if v < 0 { !(v << 1) } else { v << 1 };
    while v >= 0x20 {
        let chunk = (0x20 | (v & 0x1f)) + 63;
        out.push(char::from_u32(chunk as u32).unwrap_or('?'));
        v >>= 5;
    }
    out.push(char::from_u32((v + 63) as u32).unwrap_or('?'));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_polyline_is_empty_string() {
        assert_eq!(encode_polyline(&[]), "");
    }

    #[test]
    fn single_point_encodes_to_known_value() {
        // (38.5, -120.2) — matches Google's reference output `_p~iF~ps|U`.
        let pts = vec![HistoryPoint {
            lat: 38.5,
            lng: -120.2,
            timestamp: None,
            speed: 0,
            speed_limit: 0,
        }];
        assert_eq!(encode_polyline(&pts), "_p~iF~ps|U");
    }

    #[test]
    fn test_compression_idling() {
        let mut pts = Vec::new();
        // 10 points at the same spot
        for _ in 0..10 {
            pts.push(HistoryPoint {
                lat: 30.0, lng: 31.0, timestamp: None, speed: 0, speed_limit: 80
            });
        }
        let simplified = simplify_points(&pts, &[], &[]);
        // Should be reduced to 2 points (first and last)
        assert_eq!(simplified.len(), 2);
    }

    #[test]
    fn test_straight_line_compression() {
        let mut pts = Vec::new();
        // 20 points in a perfectly straight line, 5m apart (~100m total)
        for i in 0..20 {
            pts.push(HistoryPoint {
                lat: 30.0 + (i as f64 * 0.000045), 
                lng: 31.0, 
                timestamp: None, 
                speed: 60, 
                speed_limit: 80
            });
        }
        let simplified = simplify_points(&pts, &[], &[]);
        // 100m straight should be compressed significantly (down to ~2-3 points)
        assert!(simplified.len() < 5);
    }

    #[test]
    fn test_stop_preservation() {
        use crate::domain::time::utc_now;
        let now = utc_now();
        let mut pts = Vec::new();
        // 10 points at the same spot, with timestamps
        for i in 0..10 {
            pts.push(HistoryPoint {
                lat: 30.0, 
                lng: 31.0, 
                timestamp: Some(now + chrono::Duration::seconds(i as i64)), 
                speed: 0, 
                speed_limit: 80
            });
        }
        
        // Stop in the middle (at 5th second)
        let stop = Stop {
            lat: 30.0, lng: 31.0, 
            from: now + chrono::Duration::seconds(5), 
            to: now + chrono::Duration::seconds(6),
            duration: "1s".into(), address: "".into()
        };
        
        let simplified = simplify_points(&pts, &[stop], &[]);
        // Should be reduced to 4 points: first, stop_from, stop_to, last.
        // (In this case, since it's all same spot, radial filter would normally remove everything but first/last).
        assert_eq!(simplified.len(), 4);
        assert_eq!(simplified[1].timestamp, Some(now + chrono::Duration::seconds(5)));
        assert_eq!(simplified[2].timestamp, Some(now + chrono::Duration::seconds(6)));
    }

    #[test]
    fn test_ignition_preservation() {
        use crate::domain::time::utc_now;
        let now = utc_now();
        let mut pts = Vec::new();
        for i in 0..10 {
            pts.push(HistoryPoint {
                lat: 30.0, 
                lng: 31.0, 
                timestamp: Some(now + chrono::Duration::seconds(i as i64)), 
                speed: 0, 
                speed_limit: 80
            });
        }
        
        let sensor = SensorEvent {
            lat: 30.0, lng: 31.0, 
            timestamp: now + chrono::Duration::seconds(3),
            type_name: "Ignition On".into()
        };
        
        let simplified = simplify_points(&pts, &[], &[sensor]);
        // Should be 3 points: first, ignition, last.
        assert_eq!(simplified.len(), 3);
        assert_eq!(simplified[1].timestamp, Some(now + chrono::Duration::seconds(3)));
    }
}
