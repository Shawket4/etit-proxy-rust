//! Best delivery order for a multi-drop trip, from one distance matrix.
//!
//! The scan asks Valhalla for one truck distance matrix over
//! `[terminal, drop_1 … drop_n]` (drops in the order driven), at the trip's
//! departure time. From it, without further calls:
//!
//! - the driven order's distance and the best order's, by trying every order
//!   (n ≤ [`MAX_DROPS`]: 7! = 5040 orders, microseconds);
//! - the bundling check: is one truck visiting every drop meaningfully
//!   shorter than separate out-and-back trips? The matrix is directed, so
//!   each return is the real drop → terminal distance, not the outbound one
//!   doubled.

/// Above this, trying every order stops being trivial; such trips are rare.
pub const MAX_DROPS: usize = 7;

#[derive(Debug, Clone, PartialEq)]
pub struct OrderCheck {
    /// Distance of the order driven, terminal → last drop, km.
    pub driven_km: f64,
    /// Best order, as indices into the drops (0-based, driven order).
    pub best_order: Vec<usize>,
    pub best_km: f64,
    /// Best order plus the return from its last drop, km.
    pub bundled_km: f64,
    /// Separate trips: terminal → drop → terminal for each drop, km.
    pub split_km: f64,
}

/// `m[i][j]`: km from point i to point j; point 0 is the terminal.
/// `None` when there are fewer than 2 or more than [`MAX_DROPS`] drops, or
/// the matrix lacks a distance the check needs.
pub fn check_order(m: &[Vec<Option<f64>>]) -> Option<OrderCheck> {
    let n = m.len().checked_sub(1)?;
    if !(2..=MAX_DROPS).contains(&n) || m.iter().any(|row| row.len() != m.len()) {
        return None;
    }
    let d = |i: usize, j: usize| m[i][j];
    let path_km = |order: &[usize]| -> Option<f64> {
        let mut total = 0.0;
        let mut prev = 0usize;
        for &x in order {
            total += d(prev, x)?;
            prev = x;
        }
        Some(total)
    };

    let driven: Vec<usize> = (1..=n).collect();
    let driven_km = path_km(&driven)?;

    let mut best_km = driven_km;
    let mut best: Vec<usize> = driven.clone();
    let mut perm = driven.clone();
    // Heap's algorithm, iterative.
    let mut c = vec![0usize; n];
    let mut i = 0;
    while i < n {
        if c[i] < i {
            if i % 2 == 0 {
                perm.swap(0, i);
            } else {
                perm.swap(c[i], i);
            }
            if let Some(km) = path_km(&perm) {
                if km < best_km - 1e-9 {
                    best_km = km;
                    best = perm.clone();
                }
            }
            c[i] += 1;
            i = 0;
        } else {
            c[i] = 0;
            i += 1;
        }
    }

    let bundled_km = best_km + d(*best.last()?, 0)?;
    let mut split_km = 0.0;
    for x in 1..=n {
        split_km += d(0, x)? + d(x, 0)?;
    }

    Some(OrderCheck {
        driven_km,
        best_order: best.iter().map(|x| x - 1).collect(),
        best_km,
        bundled_km,
        split_km,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Points on a line (km positions), distance = |a - b|, optionally with
    /// a one-way surcharge on returns toward the terminal.
    fn line(pos: &[f64], return_extra: f64) -> Vec<Vec<Option<f64>>> {
        pos.iter()
            .map(|&a| {
                pos.iter()
                    .map(|&b| Some((a - b).abs() + if b < a { return_extra } else { 0.0 }))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn driving_far_first_then_back_is_beaten_by_near_first() {
        // Terminal at 0, drops driven 50 then 20: 50 + 30 = 80; best 20 then 50 = 50.
        let c = check_order(&line(&[0.0, 50.0, 20.0], 0.0)).unwrap();
        assert_eq!(c.driven_km, 80.0);
        assert_eq!(c.best_km, 50.0);
        assert_eq!(c.best_order, vec![1, 0]);
        assert_eq!(c.bundled_km, 100.0);
        assert_eq!(c.split_km, 140.0);
    }

    #[test]
    fn the_driven_order_wins_ties() {
        let c = check_order(&line(&[0.0, 10.0, 20.0, 30.0], 0.0)).unwrap();
        assert_eq!(c.best_order, vec![0, 1, 2]);
        assert_eq!(c.best_km, c.driven_km);
    }

    #[test]
    fn returns_use_the_real_one_way_distance() {
        // Returning toward the terminal costs 5 km extra (a one-way detour).
        let c = check_order(&line(&[0.0, 10.0, 20.0], 5.0)).unwrap();
        assert_eq!(c.bundled_km, 20.0 + 25.0);
        assert_eq!(c.split_km, (10.0 + 15.0) + (20.0 + 25.0));
    }

    #[test]
    fn seven_drops_are_searched_exhaustively() {
        // Driven in a zigzag; the best is in order of position.
        let c = check_order(&line(&[0.0, 70.0, 10.0, 60.0, 20.0, 50.0, 30.0, 40.0], 0.0)).unwrap();
        assert_eq!(c.best_km, 70.0);
        assert_eq!(c.best_order, vec![1, 3, 5, 6, 4, 2, 0]);
    }

    #[test]
    fn one_drop_or_too_many_or_a_gap_is_no_check() {
        assert!(check_order(&line(&[0.0, 10.0], 0.0)).is_none());
        assert!(check_order(&line(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], 0.0)).is_none());
        let mut m = line(&[0.0, 10.0, 20.0], 0.0);
        m[0][1] = None;
        assert!(check_order(&m).is_none(), "the driven order itself is unroutable");
    }

    #[test]
    fn an_unroutable_alternative_is_skipped_not_fatal() {
        let mut m = line(&[0.0, 50.0, 20.0], 0.0);
        m[0][2] = None; // cannot start with the near drop
        let c = check_order(&m);
        assert!(c.is_none(), "split trips need terminal → every drop");
        let mut m = line(&[0.0, 50.0, 20.0, 30.0], 0.0);
        m[2][1] = None;
        let c = check_order(&m).unwrap();
        assert!(c.best_km <= c.driven_km);
    }
}
