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
//! The floor alone is not enough. Two more kinds of short reading look like
//! drives but are not the whole route (both seen on Somed → Qena,
//! 2026-11-27):
//!
//! - **Partial tracks.** The tracker came online part-way, so the leg starts
//!   60–270 km down the road. [`whole_drive`] wants the shape to start and
//!   end within [`MAX_END_KM`] of the pins.
//! - **Gap chords.** A 29.8 km or a 127.5 km straight line drawn across a GPS
//!   gap. Valhalla's matched shape has a vertex at every road node, so a
//!   clean drive's longest segment is 2–3 km. [`whole_drive`] refuses any
//!   segment over [`MAX_JUMP_KM`].
//!
//! The service walks [`Pick::ordered`] from the shortest and shows the first
//! whole drive. Everything shorter (below the floor or not whole) is counted
//! as set aside and never shown as the best.
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

/// Longest straight segment a whole drive may contain.
pub const MAX_JUMP_KM: f64 = 5.0;
/// How far a whole drive's ends may be from the pins: the terminal and
/// drop-off geofences plus pin error. Partial tracks start tens of km out.
pub const MAX_END_KM: f64 = 10.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    /// Candidates at or above the floor, shortest first.
    pub ordered: Vec<Candidate>,
    /// Road-matched legs with a shape and a usable km.
    pub considered: usize,
    /// Of those, how many fell below the floor.
    pub below_floor: usize,
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
    let below_floor = usable.iter().filter(|c| c.km < floor).count();
    Pick {
        considered: usable.len(),
        ordered: usable.into_iter().filter(|c| c.km >= floor).collect(),
        below_floor,
        reference_km,
    }
}

/// Why a shape is not the whole route.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NotWhole {
    TooShort,
    /// A straight segment longer than [`MAX_JUMP_KM`]: a GPS gap.
    Gap,
    /// Starts or ends more than [`MAX_END_KM`] from its pin.
    Partial,
}

/// Is this matched shape one complete drive from the terminal to the
/// drop-off? Without pins only the gap check applies.
pub fn whole_drive(shape: &[(f64, f64)], pins: Option<((f64, f64), (f64, f64))>) -> Result<(), NotWhole> {
    let km = |a: (f64, f64), b: (f64, f64)| crate::zones::geo::haversine_m(a.0, a.1, b.0, b.1) / 1000.0;
    let (Some(&first), Some(&last)) = (shape.first(), shape.last()) else {
        return Err(NotWhole::TooShort);
    };
    if shape.len() < 2 {
        return Err(NotWhole::TooShort);
    }
    if shape.windows(2).any(|w| km(w[0], w[1]) > MAX_JUMP_KM) {
        return Err(NotWhole::Gap);
    }
    if let Some((from, to)) = pins {
        if km(first, from) > MAX_END_KM || km(last, to) > MAX_END_KM {
            return Err(NotWhole::Partial);
        }
    }
    Ok(())
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
        assert_eq!(first(&p), Some(535.8));
        assert_eq!((p.considered, p.below_floor), (8, 5));
        assert_eq!(p.ordered.iter().map(|c| c.km).collect::<Vec<_>>(), vec![535.8, 541.7, 544.4]);
    }

    #[test]
    fn a_trip_that_matches_the_truck_route_is_kept_even_far_below_the_typical_trip() {
        // بدر -> 36 حربي: one truck drove the 92.9 km route, most went the long way.
        let p = pick(&legs(&[106.4, 111.3, 91.6, 111.4]), Some(92.9));
        assert_eq!(first(&p), Some(91.6));
        assert_eq!(p.below_floor, 0);
        assert_eq!(p.reference_km, Some(92.9));
    }

    #[test]
    fn a_truck_route_longer_than_every_trip_does_not_discard_them() {
        // Valhalla's fastest route can be far longer than a road the trucks use.
        let p = pick(&legs(&[540.0, 545.0, 551.0, 548.0]), Some(700.0));
        assert_eq!(first(&p), Some(540.0));
        assert_eq!(p.reference_km, Some(548.0));
    }

    #[test]
    fn without_a_truck_route_the_upper_typical_trip_is_the_reference() {
        let p = pick(&legs(&[253.0, 535.0, 541.0, 544.0]), None);
        assert_eq!(first(&p), Some(535.0));
        assert_eq!((p.below_floor, p.reference_km), (1, Some(541.0)));
    }

    #[test]
    fn a_route_where_most_trackers_dropped_out_still_finds_the_real_drive() {
        let p = pick(&legs(&[250.0, 260.0, 270.0, 280.0, 290.0, 538.0, 541.0, 544.0]), None);
        assert_eq!(first(&p), Some(538.0));
        assert_eq!(p.below_floor, 5);
    }

    #[test]
    fn nothing_to_pick_is_none_not_a_zero() {
        let p = pick(&[], Some(100.0));
        assert_eq!((first(&p), p.considered, p.below_floor), (None, 0, 0));
        assert_eq!(first(&pick(&legs(&[0.0, f64::NAN]), None)), None);
    }

    fn first(p: &Pick) -> Option<f64> {
        p.ordered.first().map(|c| c.km)
    }

    /// A road east from (30.0, 31.0), a vertex every ~1 km.
    fn road(km: usize) -> Vec<(f64, f64)> {
        (0..=km).map(|i| (30.0, 31.0 + i as f64 * 0.0104)).collect()
    }

    #[test]
    fn a_complete_clean_drive_is_whole() {
        let r = road(60);
        let pins = ((30.0, 31.0), (30.0, 31.624));
        assert_eq!(whole_drive(&r, Some(pins)), Ok(()));
        assert_eq!(whole_drive(&r, None), Ok(()));
    }

    #[test]
    fn a_straight_line_across_a_gps_gap_is_not_whole() {
        let mut r = road(60);
        r.drain(20..50); // ~31 km bridged by one straight segment
        assert_eq!(whole_drive(&r, None), Err(NotWhole::Gap));
    }

    #[test]
    fn a_track_that_starts_part_way_is_not_whole() {
        let r = road(60);
        // The terminal pin is 25 km west of where the track begins.
        let pins = ((30.0, 30.74), (30.0, 31.624));
        assert_eq!(whole_drive(&r, Some(pins)), Err(NotWhole::Partial));
        // Ends near the pins (geofence edge, pin error) are fine.
        let near = ((30.03, 31.0), (30.0, 31.70));
        assert_eq!(whole_drive(&r, Some(near)), Ok(()));
        assert_eq!(whole_drive(&r[..1], None), Err(NotWhole::TooShort));
    }
}
