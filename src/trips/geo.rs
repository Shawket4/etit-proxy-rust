//! Geometry helpers for the trip auditor: polyline decoding and distance
//! from a point to a polyline track. Haversine lives in `zones::geo`.

use crate::zones::geo::haversine_m;

/// Decode a precision-5 encoded polyline (Google / OSRM `geometries=polyline`).
pub fn decode_polyline(encoded: &str) -> Vec<(f64, f64)> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut lat, mut lng) = (0usize, 0i64, 0i64);

    while i < bytes.len() {
        let read_varint = |i: &mut usize| -> Option<i64> {
            let (mut result, mut shift) = (0i64, 0u32);
            loop {
                if *i >= bytes.len() {
                    return None;
                }
                let b = bytes[*i] as i64 - 63;
                *i += 1;
                if b < 0 {
                    return None; // malformed input
                }
                result |= (b & 0x1f) << shift;
                shift += 5;
                if b < 0x20 {
                    break;
                }
            }
            Some(if result & 1 != 0 { !(result >> 1) } else { result >> 1 })
        };

        let Some(dlat) = read_varint(&mut i) else { break };
        let Some(dlng) = read_varint(&mut i) else { break };
        lat += dlat;
        lng += dlng;
        out.push((lat as f64 / 1e5, lng as f64 / 1e5));
    }
    out
}

/// Minimum distance in meters from a point to a polyline, using a local
/// equirectangular projection per segment (adequate at route scale).
pub fn distance_to_track_m(lat: f64, lng: f64, track: &[(f64, f64)]) -> f64 {
    if track.is_empty() {
        return f64::INFINITY;
    }
    if track.len() == 1 {
        return haversine_m(lat, lng, track[0].0, track[0].1);
    }

    let mut best = f64::INFINITY;
    for w in track.windows(2) {
        let (alat, alng) = w[0];
        let (blat, blng) = w[1];

        // Cheap bbox rejection: skip segments whose box is further than the
        // current best (padded via degree deltas).
        let pad_deg = best / 111_320.0;
        if pad_deg.is_finite() {
            let (min_lat, max_lat) = (alat.min(blat) - pad_deg, alat.max(blat) + pad_deg);
            if lat < min_lat || lat > max_lat {
                continue;
            }
        }

        let cos_lat = ((alat + blat) / 2.0).to_radians().cos();
        let ax = alng * cos_lat;
        let ay = alat;
        let bx = blng * cos_lat;
        let by = blat;
        let px = lng * cos_lat;
        let py = lat;

        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 <= f64::EPSILON {
            0.0
        } else {
            (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
        };
        let (cx, cy) = (ax + t * dx, ay + t * dy);
        let d = haversine_m(lat, lng, cy, cx / cos_lat);
        if d < best {
            best = d;
        }
    }
    best
}

/// Sum of haversine distances over consecutive points, in km. Jumps larger
/// than `max_jump_m` (GPS glitches, tracker gaps) are skipped so one bad fix
/// doesn't inflate a leg by tens of kilometers.
pub fn track_length_km(points: &[(f64, f64)], max_jump_m: f64) -> f64 {
    let mut total = 0.0;
    for w in points.windows(2) {
        let d = haversine_m(w[0].0, w[0].1, w[1].0, w[1].1);
        if d <= max_jump_m {
            total += d;
        }
    }
    total / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_matches_reference() {
        // Google's reference: "_p~iF~ps|U_ulLnnqC_mqNvxq`@"
        let pts = decode_polyline("_p~iF~ps|U_ulLnnqC_mqNvxq`@");
        assert_eq!(pts.len(), 3);
        assert!((pts[0].0 - 38.5).abs() < 1e-9);
        assert!((pts[0].1 - -120.2).abs() < 1e-9);
        assert!((pts[2].0 - 43.252).abs() < 1e-9);
        assert!((pts[2].1 - -126.453).abs() < 1e-9);
    }

    #[test]
    fn decode_roundtrips_with_encoder() {
        use crate::domain::history::encode_polyline;
        use crate::domain::HistoryPoint;
        let pts = vec![
            HistoryPoint { lat: 30.0444, lng: 31.2357, timestamp: None, speed: 0, speed_limit: 0 },
            HistoryPoint { lat: 30.1, lng: 31.3, timestamp: None, speed: 0, speed_limit: 0 },
            HistoryPoint { lat: 29.9, lng: 31.25, timestamp: None, speed: 0, speed_limit: 0 },
        ];
        let decoded = decode_polyline(&encode_polyline(&pts));
        assert_eq!(decoded.len(), 3);
        for (d, p) in decoded.iter().zip(&pts) {
            assert!((d.0 - p.lat).abs() < 2e-5);
            assert!((d.1 - p.lng).abs() < 2e-5);
        }
    }

    #[test]
    fn distance_to_track_on_segment_is_small() {
        let track = vec![(30.0, 31.0), (30.0, 31.1)];
        // Point on the segment midline.
        let d = distance_to_track_m(30.0, 31.05, &track);
        assert!(d < 1.0, "expected ~0, got {d}");
        // Point ~1.11km north of the segment.
        let d = distance_to_track_m(30.01, 31.05, &track);
        assert!((d - 1113.0).abs() < 30.0, "expected ~1113m, got {d}");
    }

    #[test]
    fn track_length_skips_glitch_jumps() {
        let pts = vec![
            (30.0, 31.0),
            (30.0, 31.01),  // ~0.96 km
            (31.5, 31.01),  // glitch: ~167 km jump
            (31.5, 31.02),  // ~0.95 km
        ];
        let km = track_length_km(&pts, 5_000.0);
        assert!(km > 1.5 && km < 2.5, "glitch not skipped: {km}");
    }
}
