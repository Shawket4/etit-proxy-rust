use crate::zones::{DeadZone, PointTS, Visit};

// `points` MUST be sorted ascending by ts. A visit is a contiguous in-zone stretch;
// it only ends once the vehicle has been OUTSIDE for longer than `gap_secs` (brief
// GPS blips/edge jitter stay one visit; a genuine leave+re-enter past the gap makes
// two visits -> two alerts).
pub fn detect_visits(points: &[PointTS], zone: &DeadZone, gap_secs: i64) -> Vec<Visit> {
    let (dlat, dlng) = crate::zones::geo::bbox_deltas(zone.lat, zone.radius_m);
    let mut visits = Vec::new();
    let (mut in_visit, mut start, mut last_inside, mut count) = (false, 0i64, 0i64, 0u32);
    for p in points {
        let inside = (p.lat - zone.lat).abs() <= dlat
            && (p.lng - zone.lng).abs() <= dlng
            && crate::zones::geo::haversine_m(p.lat, p.lng, zone.lat, zone.lng) <= zone.radius_m;
        
        if inside {
            if !in_visit {
                in_visit = true;
                start = p.ts;
                count = 0;
            }
            last_inside = p.ts;
            count += 1;
        } else if in_visit && (p.ts - last_inside) > gap_secs {
            visits.push(Visit { entry: start, exit: last_inside, points: count });
            in_visit = false;
        }
    }
    if in_visit {
        visits.push(Visit { entry: start, exit: last_inside, points: count });
    }
    visits
}
