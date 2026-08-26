//! Day partitioning: split a vehicle-day's GPS into physical journeys and
//! assign them to the day's registered trips by matching details.
//!
//! The matcher audits one registered trip against one GPS window. When a
//! vehicle runs several trips a day (often from the same terminal), matching
//! each trip against the whole day collides: windows overlap, kilometers
//! double-count, and later trips starve (observed on ف ق ص 4253, 2–3 trips
//! daily). The fix: detect the day's terminal visits ONCE, derive journeys
//! (terminal exit → next terminal entry), and give every registered trip at
//! most one journey — only where the details agree: the journey departs the
//! trip's terminal, preferring the journey that visits the trip's drop-offs.

use super::matcher::owned_visits;
use crate::zones::PointTS;
use super::{Place, PlannedTrip};
use crate::zones::Visit;

/// One physical journey: from a terminal exit to the next terminal entry
/// (any terminal — the return is not guaranteed to be the departure one).
#[derive(Debug, Clone)]
pub struct Journey {
    /// Canonical name of the terminal the journey departed from.
    pub terminal_name: String,
    /// Terminal exit timestamp (journey start).
    pub start: i64,
    /// Entry timestamp of the departing terminal visit — slicing from here
    /// (not from the exit) keeps the full dwell visible to the matcher,
    /// which needs it to register the departure visit at all.
    pub depart_entry: i64,
    /// Next terminal entry (journey end). `None` = still open (no entry in
    /// the loaded GPS) — the matcher decides return_pending vs no_return.
    pub end: Option<i64>,
    /// Exit timestamp of the closing terminal visit — the slice must reach
    /// it so the return visit's dwell qualifies.
    pub end_exit: Option<i64>,
    /// Names of drop-off geofences visited inside the journey, in order.
    pub dropoffs: Vec<String>,
}

/// Detect journeys within `[window_start, window_end)` from terminal visits.
/// `points` should span the assembled multi-day window; only journeys that
/// START inside the bounds are returned (a journey may END later — that's
/// the cross-midnight case).
pub fn partition_journeys(
    points: &[PointTS],
    terminals: &[Place],
    dropoffs: &[Place],
    depart_terminals: &std::collections::HashSet<String>,
    window_start: i64,
    window_end: i64,
) -> Vec<Journey> {
    // A place can be a terminal in general but one of THIS day's delivery
    // destinations (dual-role: same name/location). Arriving there is then a
    // delivery, not a journey boundary — exclude it from the splitting set,
    // unless some trip today also DEPARTS from it (ambiguous; terminal role
    // wins so departures still work).
    let terminals: Vec<Place> = terminals
        .iter()
        .filter(|t| {
            depart_terminals.contains(&t.name)
                || !dropoffs.iter().any(|d| {
                    d.name == t.name
                        || crate::zones::geo::haversine_m(d.lat, d.lng, t.lat, t.lng) < 100.0
                })
        })
        .cloned()
        .collect();
    let terminals = terminals.as_slice();

    // All terminal visits, tagged and chronological. Terminals and drop-offs
    // compete for points in one arena, so overlapping fences (two pins across
    // the street) can't both claim the same dwell.
    let mut arena: Vec<Place> = terminals.to_vec();
    arena.extend(dropoffs.iter().cloned());
    let owned = owned_visits(points, &arena, 0);

    let mut visits: Vec<(usize, Visit)> = Vec::new();
    for (ti, vs) in owned.iter().take(terminals.len()).enumerate() {
        for v in vs {
            visits.push((ti, v.clone()));
        }
    }
    visits.sort_by_key(|(_, v)| v.entry);
    let dropoff_visits = &owned[terminals.len()..];

    let mut journeys = Vec::new();
    for i in 0..visits.len() {
        let (ti, ref v) = visits[i];
        let start = v.exit;
        if start < window_start || start >= window_end {
            continue;
        }
        let end = visits.get(i + 1).map(|(_, next)| next.entry);
        let end_exit = visits.get(i + 1).map(|(_, next)| next.exit);
        match end {
            Some(e) => {
                // Zero-travel gaps (GPS flapping at the fence) are not
                // journeys.
                if e <= start + 60 {
                    continue;
                }
            }
            None => {
                // Open-ended candidate (the day's last terminal visit): it is
                // only a journey if the truck actually LEFT — some later
                // point well outside the terminal's fence. Otherwise the
                // "exit" is just the last point of the final dwell.
                let t = &terminals[ti];
                let departed = points.iter().any(|p| {
                    p.ts > start
                        && crate::zones::geo::haversine_m(p.lat, p.lng, t.lat, t.lng)
                            > t.radius_m * 2.0
                });
                if !departed {
                    continue;
                }
            }
        }
        let d_end = end.unwrap_or(i64::MAX);
        let mut visited: Vec<(i64, String)> = Vec::new();
        for (di, d) in dropoffs.iter().enumerate() {
            for dv in &dropoff_visits[di] {
                if dv.entry >= start && dv.entry < d_end {
                    visited.push((dv.entry, d.name.clone()));
                    break;
                }
            }
        }
        visited.sort();
        journeys.push(Journey {
            terminal_name: terminals[ti].name.clone(),
            start,
            depart_entry: v.entry,
            end,
            end_exit,
            dropoffs: visited.into_iter().map(|(_, n)| n).collect(),
        });
    }
    journeys
}

/// Assign journeys to the day's registered trips. Returns, per trip (same
/// order as `trips`), the index of its journey in `journeys` — or `None`.
///
/// Rules, per journey in chronological order:
/// - only trips whose resolved terminal equals the journey's terminal are
///   candidates (a trip with an unresolved terminal never gets a journey);
/// - among candidates, the most of the trip's planned drop-offs visited by
///   the journey wins; ties go to registration order;
/// - when no candidate's drop-offs were visited at all (e.g. all its
///   drop-offs are unpinned), fall back to registration order — that is
///   today's chronological behavior, but confined to one journey.
pub fn assign_journeys(journeys: &[Journey], trips: &[&PlannedTrip]) -> Vec<Option<usize>> {
    let mut assigned: Vec<Option<usize>> = vec![None; trips.len()];

    for (ji, j) in journeys.iter().enumerate() {
        let mut best: Option<(usize, usize)> = None; // (overlap, trip_idx) — max overlap wins
        for (tidx, trip) in trips.iter().enumerate() {
            if assigned[tidx].is_some() {
                continue;
            }
            let Some(term) = trip.terminal.as_ref() else { continue };
            if term.name != j.terminal_name {
                continue;
            }
            let overlap = trip
                .deliveries
                .iter()
                .filter(|d| j.dropoffs.iter().any(|n| *n == d.drop_off_point))
                .count();
            let better = match best {
                None => true,
                // Strictly more overlap wins; equal overlap keeps the earlier
                // registered trip (stable order).
                Some((bo, _)) => overlap > bo,
            };
            if better {
                best = Some((overlap, tidx));
            }
        }
        if let Some((_, tidx)) = best {
            assigned[tidx] = Some(ji);
        }
    }
    assigned
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trips::PlannedDelivery;

    fn place(name: &str, lat: f64, lng: f64) -> Place {
        Place { name: name.into(), lat, lng, radius_m: 400.0 }
    }

    fn pts(segments: &[((f64, f64), i64, i64)]) -> Vec<PointTS> {
        // Dwell points at a location over [t0, t1], 60s cadence.
        let mut out = Vec::new();
        for ((lat, lng), t0, t1) in segments {
            let mut t = *t0;
            while t <= *t1 {
                out.push(PointTS { ts: t, lat: *lat, lng: *lng });
                t += 60;
            }
        }
        out.sort_by_key(|p| p.ts);
        out
    }

    fn trip(term: &Place, drop_name: &str, drop: Option<&Place>) -> PlannedTrip {
        PlannedTrip {
            parent_trip_id: 0,
            day_local: "2026-08-25".into(),
            company: "T".into(),
            terminal: Some(term.clone()),
            terminal_name: term.name.clone(),
            car_no_plate: String::new(),
            driver_name: String::new(),
            etit_car_id: "v".into(),
            deliveries: vec![PlannedDelivery {
                trip_id: 0,
                drop_off_point: drop_name.into(),
                place: drop.cloned(),
                expected_km: None,
            }],
        }
    }

    #[test]
    fn two_same_terminal_trips_get_their_own_journeys() {
        let term = place("بدر", 30.10, 31.70);
        let drop_a = place("A", 30.20, 31.40);
        let drop_b = place("B", 29.95, 31.55);

        // Terminal dwell → A → terminal → B → terminal.
        let points = pts(&[
            ((30.10, 31.70), 1_000, 4_000),
            ((30.20, 31.40), 8_000, 10_000),
            ((30.10, 31.70), 14_000, 16_000),
            ((29.95, 31.55), 20_000, 22_000),
            ((30.10, 31.70), 26_000, 28_000),
        ]);

        let journeys = partition_journeys(
            &points,
            &[term.clone()],
            &[drop_a.clone(), drop_b.clone()],
            &std::collections::HashSet::from([term.name.clone()]),
            0,
            i64::MAX,
        );
        assert_eq!(journeys.len(), 2, "{journeys:?}");
        assert_eq!(journeys[0].dropoffs, vec!["A".to_string()]);
        assert_eq!(journeys[1].dropoffs, vec!["B".to_string()]);

        // Registration order is REVERSED vs journey order — details must win.
        let trip_b = trip(&term, "B", Some(&drop_b));
        let trip_a = trip(&term, "A", Some(&drop_a));
        let trips: Vec<&PlannedTrip> = vec![&trip_b, &trip_a];
        let assigned = assign_journeys(&journeys, &trips);
        assert_eq!(assigned, vec![Some(1), Some(0)]);
    }

    #[test]
    fn wrong_terminal_never_assigned() {
        let term_a = place("بدر", 30.10, 31.70);
        let term_b = place("هايكستب", 30.14, 31.35);
        let drop = place("A", 30.20, 31.40);
        let points = pts(&[
            ((30.10, 31.70), 1_000, 4_000),
            ((30.20, 31.40), 8_000, 10_000),
            ((30.10, 31.70), 14_000, 16_000),
        ]);
        let journeys = partition_journeys(
            &points,
            &[term_a.clone(), term_b.clone()],
            &[drop],
            &std::collections::HashSet::from([term_a.name.clone()]),
            0,
            i64::MAX,
        );
        assert_eq!(journeys.len(), 1);

        let t = trip(&term_b, "A", None); // registered from the OTHER terminal
        let trips: Vec<&PlannedTrip> = vec![&t];
        assert_eq!(assign_journeys(&journeys, &trips), vec![None]);
    }

    #[test]
    fn overlapping_fences_do_not_create_phantom_journeys() {
        // Drop-off X sits 350 m from the terminal — inside the terminal's
        // 400 m fence. A dwell at X used to register as a terminal visit
        // too, splitting the day into phantom journeys.
        let term = place("بدر", 30.1000, 31.7000);
        let x = place("X", 30.1000 + 350.0 / 111_320.0, 31.7000);

        let points = pts(&[
            ((30.1000, 31.7000), 1_000, 4_000),                    // load at terminal
            ((30.1000 + 350.0 / 111_320.0, 31.7000), 8_000, 11_000), // deliver at X
            ((30.1000, 31.7000), 15_000, 18_000),                  // back at terminal
        ]);

        let journeys = partition_journeys(
            &points,
            &[term.clone()],
            &[x.clone()],
            &std::collections::HashSet::from([term.name.clone()]),
            0,
            i64::MAX,
        );
        assert_eq!(journeys.len(), 1, "phantom journeys: {journeys:?}");
        assert_eq!(journeys[0].dropoffs, vec!["X".to_string()]);
        assert!(journeys[0].end.is_some());
    }

    #[test]
    fn dual_role_place_is_a_delivery_not_a_journey_boundary() {
        // "انابيب البترول بدر" is a terminal in general, but today's trip
        // DELIVERS to it from بدر. Arriving there must not end the journey;
        // the journey ends back at بدر.
        let badr = place("بدر", 30.1000, 31.7000);
        let pipelines = place("انابيب البترول بدر", 30.1600, 31.7600);
        let dual_dropoff = place("انابيب البترول بدر", 30.1600, 31.7600);

        let points = pts(&[
            ((30.1000, 31.7000), 1_000, 4_000),   // load at بدر
            ((30.1600, 31.7600), 8_000, 11_000),  // deliver at the pipelines
            ((30.1000, 31.7000), 15_000, 18_000), // back at بدر
        ]);

        let journeys = partition_journeys(
            &points,
            &[badr.clone(), pipelines.clone()],
            &[dual_dropoff],
            &std::collections::HashSet::from([badr.name.clone()]),
            0,
            i64::MAX,
        );
        assert_eq!(journeys.len(), 1, "{journeys:?}");
        assert_eq!(journeys[0].terminal_name, "بدر");
        assert_eq!(journeys[0].dropoffs, vec!["انابيب البترول بدر".to_string()]);
        assert!(journeys[0].end.is_some(), "journey must close back at بدر");
    }

    #[test]
    fn unpinned_dropoffs_fall_back_to_registration_order() {
        let term = place("بدر", 30.10, 31.70);
        let points = pts(&[
            ((30.10, 31.70), 1_000, 4_000),
            ((30.20, 31.40), 8_000, 10_000),
            ((30.10, 31.70), 14_000, 16_000),
            ((29.95, 31.55), 20_000, 22_000),
            ((30.10, 31.70), 26_000, 28_000),
        ]);
        let journeys = partition_journeys(
            &points,
            &[term.clone()],
            &[],
            &std::collections::HashSet::from([term.name.clone()]),
            0,
            i64::MAX,
        );
        assert_eq!(journeys.len(), 2);

        let t1 = trip(&term, "X", None);
        let t2 = trip(&term, "Y", None);
        let trips: Vec<&PlannedTrip> = vec![&t1, &t2];
        // No pins to disambiguate → chronological by registration order.
        assert_eq!(assign_journeys(&journeys, &trips), vec![Some(0), Some(1)]);
    }
}
