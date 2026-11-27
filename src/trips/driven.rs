//! Driven km for one terminal → drop-off route, from the audit's own legs.
//!
//! The fee lookup (falcon-fees) shows a route's recorded km and its road-route km.
//! This is the third number: how far our trucks actually drove it. The source
//! is the outbound legs the trip audit already stores. An outbound leg runs
//! from the terminal geofence exit to the first drop-off geofence entry, so
//! its km is one uninterrupted drive of exactly that route.
//!
//! Only outbound legs count:
//! - a `between` leg starts at the previous drop, not at the terminal;
//! - a `return` leg runs the other way, and one-way roads and time-of-day
//!   bans make the reverse a different route;
//! - an outbound leg with a garage dwell is split at the garage, so neither
//!   half is named terminal → drop-off and the trip drops out by itself.
//!
//! Because a leg starts at the terminal geofence edge and ends at the
//! drop-off geofence edge, it is a few hundred metres shorter than a
//! gate-to-gate distance. That only matters on very short routes.

use serde::Serialize;

/// One stored outbound leg on the route.
#[derive(Debug, Clone)]
pub struct DrivenLeg {
    pub km: f64,
    pub day_local: String,
    /// True when the km came from map matching (road-snapped) rather than the
    /// straight-chord sum of GPS fixes.
    pub road_matched: bool,
}

/// What the lookup returns. Every field is always present; the km and day
/// fields are `null` when `trips` is 0.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DrivenSummary {
    pub trips: usize,
    /// The middle trip: one truck's detour does not move it.
    pub median_km: Option<f64>,
    /// 10th and 90th percentile (nearest rank), so "most trips" excludes the
    /// odd GPS glitch at either end. With few trips these are the min and max.
    pub p10_km: Option<f64>,
    pub p90_km: Option<f64>,
    /// How many of `trips` were road-matched (the rest are chord sums).
    pub road_matched: usize,
    pub first_day: Option<String>,
    pub latest_day: Option<String>,
}

pub fn summarise(legs: &[DrivenLeg]) -> DrivenSummary {
    // A zero or non-finite km is a leg with no usable GPS inside it, not a
    // drive of zero km.
    let usable: Vec<&DrivenLeg> = legs
        .iter()
        .filter(|l| l.km.is_finite() && l.km > 0.0)
        .collect();

    let mut kms: Vec<f64> = usable.iter().map(|l| l.km).collect();
    kms.sort_by(|a, b| a.total_cmp(b));

    let n = kms.len();
    let median = match n {
        0 => None,
        _ if n % 2 == 1 => Some(kms[n / 2]),
        _ => Some((kms[n / 2 - 1] + kms[n / 2]) / 2.0),
    };

    // Days are YYYY-MM-DD, so string order is date order.
    let first_day = usable
        .iter()
        .map(|l| l.day_local.as_str())
        .min()
        .map(str::to_string);
    let latest_day = usable
        .iter()
        .map(|l| l.day_local.as_str())
        .max()
        .map(str::to_string);

    DrivenSummary {
        trips: n,
        median_km: median.map(round1),
        p10_km: nearest_rank(&kms, 0.10).map(round1),
        p90_km: nearest_rank(&kms, 0.90).map(round1),
        road_matched: usable.iter().filter(|l| l.road_matched).count(),
        first_day,
        latest_day,
    }
}

/// Nearest-rank percentile of an ascending slice.
fn nearest_rank(sorted: &[f64], p: f64) -> Option<f64> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    let rank = (p * n as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, n) - 1])
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg(km: f64, day: &str, matched: bool) -> DrivenLeg {
        DrivenLeg {
            km,
            day_local: day.into(),
            road_matched: matched,
        }
    }

    #[test]
    fn no_legs_is_zero_trips_with_null_figures() {
        let s = summarise(&[]);
        assert_eq!(
            s,
            DrivenSummary {
                trips: 0,
                median_km: None,
                p10_km: None,
                p90_km: None,
                road_matched: 0,
                first_day: None,
                latest_day: None,
            }
        );
    }

    #[test]
    fn one_detour_does_not_move_the_middle_trip() {
        let legs = vec![
            leg(118.0, "2026-09-01", true),
            leg(121.0, "2026-09-03", true),
            leg(119.5, "2026-09-02", false),
            leg(240.0, "2026-09-04", true), // went somewhere else on the way
            leg(120.2, "2026-09-05", true),
        ];
        let s = summarise(&legs);
        assert_eq!(s.trips, 5);
        assert_eq!(s.median_km, Some(120.2));
        assert_eq!(s.road_matched, 4);
        assert_eq!(s.first_day.as_deref(), Some("2026-09-01"));
        assert_eq!(s.latest_day.as_deref(), Some("2026-09-05"));
    }

    #[test]
    fn even_count_averages_the_two_middle_trips() {
        let legs = vec![
            leg(100.0, "d", true),
            leg(110.0, "d", true),
            leg(104.0, "d", true),
            leg(106.0, "d", true),
        ];
        assert_eq!(summarise(&legs).median_km, Some(105.0));
    }

    #[test]
    fn percentiles_trim_the_ends_once_there_are_enough_trips() {
        // 1..=20 km: nearest-rank p10 = 2nd value, p90 = 18th value.
        let legs: Vec<DrivenLeg> = (1..=20).map(|k| leg(k as f64, "d", true)).collect();
        let s = summarise(&legs);
        assert_eq!(s.p10_km, Some(2.0));
        assert_eq!(s.p90_km, Some(18.0));
    }

    #[test]
    fn with_few_trips_the_range_is_min_to_max() {
        let legs = vec![leg(130.0, "d", true), leg(125.0, "d", true)];
        let s = summarise(&legs);
        assert_eq!((s.p10_km, s.p90_km), (Some(125.0), Some(130.0)));

        let one = summarise(&[leg(77.7, "2026-10-01", false)]);
        assert_eq!(
            (one.trips, one.median_km, one.p10_km, one.p90_km),
            (1, Some(77.7), Some(77.7), Some(77.7))
        );
    }

    #[test]
    fn empty_legs_are_not_counted_as_trips() {
        let legs = vec![
            leg(0.0, "2026-08-01", false),
            leg(f64::NAN, "2026-08-02", false),
            leg(50.0, "2026-09-09", true),
        ];
        let s = summarise(&legs);
        assert_eq!(s.trips, 1);
        assert_eq!(s.median_km, Some(50.0));
        // The empty legs' days must not widen the date range either.
        assert_eq!(s.first_day.as_deref(), Some("2026-09-09"));
    }

    #[test]
    fn figures_are_rounded_to_one_decimal() {
        let s = summarise(&[leg(123.456, "d", true)]);
        assert_eq!(s.median_km, Some(123.5));
    }
}
