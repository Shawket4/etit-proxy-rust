const EARTH_R_M: f64 = 6_371_000.0;

pub fn haversine_m(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dlat = (lat2 - lat1).to_radians();
    let dlng = (lng2 - lng1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + p1.cos() * p2.cos() * (dlng / 2.0).sin().powi(2);
    2.0 * EARTH_R_M * a.sqrt().atan2((1.0 - a).sqrt())
}

// Cheap bounding-box half-widths in degrees for a zone radius; reject far points
// before doing the trig.
pub fn bbox_deltas(zone_lat: f64, radius_m: f64) -> (f64, f64) {
    let dlat = radius_m / 111_320.0;
    let coslat = zone_lat.to_radians().cos().abs().max(1e-6);
    let dlng = radius_m / (111_320.0 * coslat);
    (dlat, dlng)
}
