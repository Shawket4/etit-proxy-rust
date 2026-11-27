//! "Our trucks' best trip" on one terminal → drop-off route: the shortest real
//! drive the audit holds, which the fee lookup's map draws beside Valhalla's
//! truck route for the same two pins.
//!
//! Shortest is the pick the owner asked for, and it is also the pick most
//! exposed to bad data. A matched leg's km includes the straight line
//! Valhalla draws across a GPS gap. So a truck whose tracker went quiet for
//! 300 km "drove" Somed → Qena in 253 km, when the typical trip is 542 km
//! (seen 2026-11-27). A candidate must therefore reach [`MIN_SHARE`] of a
//! reference distance. The reference is the shorter of:
//! - Valhalla's truck route between the pins, when there is one;
//! - the audit's upper-typical trip (75th percentile).
//!
//! Each guards the other's failure:
//! - The percentile stays put even when most readings on a route are gaps;
//!   a median would sink with them and let a gap through.
//! - Taking the shorter keeps a genuinely shorter road the trucks found
//!   (Valhalla optimises time, not km) from being thrown out.
//!
//! Readings below the floor are counted as set aside and never shown as the
//! best.
//!
//! Only road-matched outbound legs with a stored shape are candidates: a
//! chord-sum leg has no road shape to draw. See [`crate::trips::driven`] for
//! why outbound legs are the ones that measure the route.

/// A drive shorter than this share of the reference is a GPS gap, not a
/// better road.
pub const MIN_SHARE: f64 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    /// `trip_match_legs.id`.
    pub id: i64,
    pub km: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub best: Option<Candidate>,
    /// Road-matched legs with a shape and a usable km.
    pub considered: usize,
    /// Of those, how many fell below the floor.
    pub set_aside: usize,
    /// The distance the floor was taken from; None with no candidates and
    /// no truck route.
    pub reference_km: Option<f64>,
}

pub fn pick(candidates: &[Candidate], truck_route_km: Option<f64>) -> Pick {
    let mut usable: Vec<Candidate> = candidates
        .iter()
        .copied()
        .filter(|c| c.km.is_finite() && c.km > 0.0)
        .collect();
    usable.sort_by(|a, b| a.km.total_cmp(&b.km));

    // Nearest-rank 75th percentile.
    let upper = (!usable.is_empty()).then(|| usable[(usable.len() * 3).div_ceil(4) - 1].km);
    let truck = truck_route_km.filter(|k| k.is_finite() && *k > 0.0);
    let reference_km = match (truck, upper) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };

    let floor = reference_km.map_or(0.0, |r| r * MIN_SHARE);
    let set_aside = usable.iter().filter(|c| c.km < floor).count();
    Pick {
        best: usable.iter().find(|c| c.km >= floor).copied(),
        considered: usable.len(),
        set_aside,
        reference_km,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legs(kms: &[f64]) -> Vec<Candidate> {
        kms.iter().enumerate().map(|(i, &km)| Candidate { id: i as i64 + 1, km }).collect()
    }

    #[test]
    fn gps_gaps_are_set_aside_and_the_shortest_real_drive_wins() {
        // Somed -> Qena as it stood on 2026-11-27: five gap readings under a
        // ~540 km route.
        let p = pick(&legs(&[541.7, 253.0, 544.4, 321.1, 326.0, 535.8, 332.9, 372.4]), Some(548.0));
        assert_eq!(p.best.map(|c| c.km), Some(535.8));
        assert_eq!((p.considered, p.set_aside), (8, 5));
    }

    #[test]
    fn a_trip_that_matches_the_truck_route_is_kept_even_far_below_the_typical_trip() {
        // بدر -> 36 حربي: one truck drove the 92.9 km route, most went the long way.
        let p = pick(&legs(&[106.4, 111.3, 91.6, 111.4]), Some(92.9));
        assert_eq!(p.best.map(|c| c.km), Some(91.6));
        assert_eq!(p.set_aside, 0);
        assert_eq!(p.reference_km, Some(92.9));
    }

    #[test]
    fn a_truck_route_longer_than_every_trip_does_not_discard_them() {
        // Valhalla's fastest route can be far longer than a road the trucks use.
        let p = pick(&legs(&[540.0, 545.0, 551.0, 548.0]), Some(700.0));
        assert_eq!(p.best.map(|c| c.km), Some(540.0));
        assert_eq!(p.reference_km, Some(548.0));
    }

    #[test]
    fn without_a_truck_route_the_upper_typical_trip_is_the_reference() {
        let p = pick(&legs(&[253.0, 535.0, 541.0, 544.0]), None);
        assert_eq!(p.best.map(|c| c.km), Some(535.0));
        assert_eq!((p.set_aside, p.reference_km), (1, Some(541.0)));
    }

    #[test]
    fn a_route_where_most_trackers_dropped_out_still_finds_the_real_drive() {
        let p = pick(&legs(&[250.0, 260.0, 270.0, 280.0, 290.0, 538.0, 541.0, 544.0]), None);
        assert_eq!(p.best.map(|c| c.km), Some(538.0));
        assert_eq!(p.set_aside, 5);
    }

    #[test]
    fn nothing_to_pick_is_none_not_a_zero() {
        let p = pick(&[], Some(100.0));
        assert_eq!((p.best, p.considered, p.set_aside), (None, 0, 0));
        assert_eq!(pick(&legs(&[0.0, f64::NAN]), None).best, None);
    }
}
