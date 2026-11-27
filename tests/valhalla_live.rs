//! The routing client against a real Valhalla built by `valhalla/build.sh`.
//!
//! Skipped (passes trivially) unless VALHALLA_URL is set:
//!
//!   VALHALLA_URL=http://127.0.0.1:18002 \
//!   VALHALLA_TIME_BANS=~/work/valhalla-build/out/apex-time-bans.json \
//!   cargo test --test valhalla_live -- --nocapture
//!
//! These check the behaviour the audit relies on, end to end: the roads
//! closed to the fleet (26th of July, the Ahmed Hamdi tunnels) avoided at
//! every hour, a matched drive along either raising a banned-road episode,
//! and the matrix/optimizer answering in the shapes the code expects.

use chrono::TimeZone;
use etit_proxy::routing::{Departure, RoutingClient, Truck};
use etit_proxy::trips::bans_check::banned_use;

const TZ: chrono_tz::Tz = chrono_tz::Africa::Cairo;
const SHEIKH_ZAYED: (f64, f64) = (30.045, 30.98);
const MOHANDESSIN: (f64, f64) = (30.056, 31.20);
const MOSTOROD: (f64, f64) = (30.146, 31.286);
const SUEZ: (f64, f64) = (29.9737, 32.5263);
const RAS_SUDR: (f64, f64) = (29.5907, 32.7080);

// Recorded drives, as GPS fixes. Taken from truck routes on a build without
// these closures, so they really are on the closed roads.
// Mostorod -> Sheikh Zayed over the 26th of July corridor, a fix every 1.2 km.
// Suez -> Ras Sudr through the original Ahmed Hamdi tunnel, a fix every 600 m
// with none inside the tunnel (GPS drops out underground).
const CORRIDOR_DRIVE: &[(f64, f64)] = &[
    (30.14593, 31.28598), (30.13706, 31.28487), (30.13439, 31.27528), (30.13862, 31.26436),
    (30.14734, 31.25674), (30.14993, 31.25069), (30.14986, 31.23676), (30.14898, 31.22120),
    (30.14311, 31.20963), (30.13239, 31.20332), (30.12177, 31.20136), (30.11088, 31.20346),
    (30.09918, 31.20090), (30.08833, 31.19535), (30.08021, 31.18520), (30.07305, 31.17602),
    (30.06290, 31.17278), (30.06538, 31.15686), (30.06727, 31.14342), (30.06604, 31.13040),
    (30.06380, 31.10748), (30.06286, 31.09468), (30.05905, 31.08201), (30.05368, 31.06824),
    (30.04904, 31.05625), (30.04317, 31.04560), (30.03711, 31.03516), (30.02952, 31.02356),
    (30.02294, 31.01348), (30.02422, 31.00328), (30.03014, 30.99350), (30.03493, 30.98207),
    (30.04351, 30.97866), (30.04506, 30.98014),
];

const TUNNEL_DRIVE: &[(f64, f64)] = &[
    (30.03102, 32.55146), (30.03624, 32.55326), (30.04108, 32.55649), (30.04655, 32.55756),
    (30.05177, 32.55574), (30.05680, 32.55308), (30.06187, 32.55045), (30.06690, 32.54822),
    (30.07288, 32.54731), (30.07900, 32.54664), (30.08743, 32.54579), (30.09280, 32.54500),
    (30.09812, 32.54307), (30.10324, 32.54138), (30.10334, 32.54680), (30.09999, 32.55174),
    (30.09913, 32.55754), (30.08385, 32.57604), (30.08050, 32.58061), (30.08073, 32.58705),
    (30.08103, 32.59354), (30.08156, 32.60083), (30.08206, 32.60828), (30.08226, 32.61490),
    (30.07819, 32.61946), (30.07137, 32.62468), (30.06593, 32.62822), (30.06051, 32.62865),
    (30.05543, 32.62854),
];

fn client() -> Option<RoutingClient> {
    let url = std::env::var("VALHALLA_URL").ok()?;
    let bans = std::env::var("VALHALLA_TIME_BANS").ok().map(std::path::PathBuf::from);
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(60)).build().unwrap();
    let c = RoutingClient::new(http, url, Truck::default(), TZ, bans);
    assert!(!c.bans().is_empty(), "set VALHALLA_TIME_BANS to the build's apex-time-bans.json");
    Some(c)
}

fn cairo(h: u32, m: u32) -> i64 {
    TZ.with_ymd_and_hms(2026, 11, 28, h, m, 0).single().unwrap().timestamp()
}

/// Fixes a minute apart, starting at `start`.
fn timed(fixes: &[(f64, f64)], start: i64) -> Vec<(f64, f64, i64)> {
    fixes.iter().enumerate().map(|(i, p)| (p.0, p.1, start + 60 * i as i64)).collect()
}

/// A route's shape as GPS fixes about every 1.2 km.
fn as_fixes(geometry: &str) -> Vec<(f64, f64)> {
    let path = etit_proxy::trips::geo::decode_polyline(geometry);
    let mut fixes = vec![path[0]];
    let mut acc = 0.0;
    for w in path.windows(2) {
        acc += etit_proxy::zones::geo::haversine_m(w[0].0, w[0].1, w[1].0, w[1].1);
        if acc >= 1200.0 {
            fixes.push(w[1]);
            acc = 0.0;
        }
    }
    fixes.push(*path.last().unwrap());
    fixes
}

/// Ban ids a truck driving this route would be flagged for.
async fn bans_driven(c: &RoutingClient, geometry: &str, start: i64) -> Vec<String> {
    let m = c.match_trace(&timed(&as_fixes(geometry), start)).await.unwrap();
    banned_use(&m.edges, &c.bans(), &TZ).into_iter().map(|u| u.ban_id).collect()
}

#[tokio::test]
async fn the_corridor_is_avoided_at_every_hour() {
    let Some(c) = client() else { return };
    let day = c.route(SHEIKH_ZAYED, MOHANDESSIN, &[], Departure::At(cairo(13, 0))).await.unwrap();
    let night = c.route(SHEIKH_ZAYED, MOHANDESSIN, &[], Departure::At(cairo(23, 0))).await.unwrap();
    let any = c.route(SHEIKH_ZAYED, MOHANDESSIN, &[], Departure::AnyHour).await.unwrap();
    println!("day {:.1} km, night {:.1} km, any hour {:.1} km", day.distance_km, night.distance_km, any.distance_km);
    assert!((day.distance_km - night.distance_km).abs() < 0.5, "closed at all hours: night is no shortcut");
    assert!((any.distance_km - day.distance_km).abs() < 0.5);
    assert_eq!(bans_driven(&c, &night.geometry, cairo(23, 0)).await, Vec::<String>::new());
}

#[tokio::test]
async fn driving_the_corridor_is_flagged_day_or_night() {
    let Some(c) = client() else { return };
    for (label, start) in [("13:00", cairo(13, 0)), ("23:00", cairo(23, 0))] {
        let m = c.match_trace(&timed(CORRIDOR_DRIVE, start)).await.unwrap();
        println!("{label}: matched {:.1} km ({:.0}% of fixes)", m.distance_km, m.coverage * 100.0);
        let uses = banned_use(&m.edges, &c.bans(), &TZ);
        println!("{uses:?}");
        let u = uses.iter().find(|u| u.ban_id == "26th-of-july").expect("a 26th of July episode");
        assert!(u.always && u.hours.is_none());
        assert!(u.km > 5.0, "most of the drive is on the corridor: {} km", u.km);
    }
}

#[tokio::test]
async fn the_ahmed_hamdi_tunnels_are_avoided_and_their_use_flagged() {
    let Some(c) = client() else { return };
    let r = c.route(SUEZ, RAS_SUDR, &[], Departure::At(cairo(3, 0))).await.unwrap();
    println!("Suez -> Ras Sudr without the tunnels: {:.1} km", r.distance_km);
    // Through the tunnel it is 84 km; any other crossing is further north.
    assert!(r.distance_km > 100.0, "{:.1} km: is the tunnel still open to trucks?", r.distance_km);
    assert_eq!(bans_driven(&c, &r.geometry, cairo(3, 0)).await, Vec::<String>::new());

    let m = c.match_trace(&timed(TUNNEL_DRIVE, cairo(3, 0))).await.unwrap();
    let uses = banned_use(&m.edges, &c.bans(), &TZ);
    println!("{uses:?}");
    let u = uses.iter().find(|u| u.ban_id == "ahmed-hamdi-tunnel").expect("the tunnel, with no fix inside it");
    assert!(u.always);
    assert!(u.km > 1.0, "{} km", u.km);
}

#[tokio::test]
async fn matrix_and_optimizer_answer_for_several_drops() {
    let Some(c) = client() else { return };
    let tanta = (30.7865, 31.0004);
    let banha = (30.466, 31.1858);
    let zagazig = (30.5877, 31.502);
    let m = c.matrix(&[MOSTOROD, tanta, banha, zagazig], Departure::At(cairo(10, 0))).await.unwrap();
    assert!(m[0][1].unwrap() > 60.0 && m[0][1].unwrap() < 140.0, "{m:?}");
    assert!(m[1][0].is_some(), "returns are in the matrix too");
    let check = etit_proxy::trips::drop_order::check_order(&m).unwrap();
    println!("drop order: {check:?}");

    let trip = c.optimize(MOSTOROD, &[tanta, banha, zagazig], MOSTOROD, Departure::At(cairo(10, 0))).await.unwrap();
    println!("optimized order {:?}, {:.1} km", trip.order, trip.distance_km);
    let mut sorted = trip.order.clone();
    sorted.sort();
    assert_eq!(sorted, vec![0, 1, 2]);
    assert_eq!(trip.legs.len(), 4);
    let whole = etit_proxy::trips::geo::decode_polyline(&trip.geometry);
    let parts: usize = trip.legs.iter().map(|l| etit_proxy::trips::geo::decode_polyline(&l.geometry).len()).sum();
    assert!(whole.len() > 100 && whole.len() <= parts, "whole-trip shape joins the legs: {} vs {parts}", whole.len());
}

const SOMED: (f64, f64) = (29.594416, 32.329073);
const QENA: (f64, f64) = (26.178375, 32.686582);

/// The fee lookup's road list, on the busiest route: a handful of major
/// roads that add up to the route and tile its geometry, and the same roads
/// again when the route is driven and map-matched like a real trip.
#[tokio::test]
async fn a_long_route_reads_as_a_few_major_roads_driven_or_routed() {
    let Some(c) = client() else { return };
    let r = c.route(SOMED, QENA, &[], Departure::AnyHour).await.unwrap();
    let roads = c.roads(&r.geometry).await.unwrap();
    for s in &roads {
        println!("{:6.1} km  {:?} [{:?}] ({}) {}-{}", s.km, s.name, s.road_ref, s.road_class, s.from_index, s.to_index);
    }
    let n = etit_proxy::trips::geo::decode_polyline(&r.geometry).len();
    assert!((2..=20).contains(&roads.len()), "{} stretches", roads.len());
    let total: f64 = roads.iter().map(|s| s.km).sum();
    assert!((total - r.distance_km).abs() < 2.0, "{total:.1} km listed for a {:.1} km route", r.distance_km);
    assert_eq!((roads[0].from_index, roads.last().unwrap().to_index), (0, n - 1));
    for w in roads.windows(2) {
        assert_eq!(w[0].to_index, w[1].from_index);
        assert!(w[0].from_index < w[0].to_index);
    }
    assert!(roads.iter().all(|s| s.km >= 3.0), "short pieces are folded in");
    // Into Qena on the Assiut–Qena road (inland) or Qena–Safaga (the coast).
    let last = roads.last().unwrap().road_ref.as_deref();
    assert!(matches!(last, Some("25") | Some("244")), "{last:?}");

    // Driven: GPS fixes along it, map-matched as the audit matches trips.
    let matched = c.match_trace(&timed(&as_fixes(&r.geometry), cairo(3, 0))).await.unwrap();
    let driven = c.roads(&matched.geometries[0]).await.unwrap();
    let d = etit_proxy::routing::roads::compare(&driven, &roads);
    println!("{d:?}");
    assert!(d.best_more.is_empty() && d.truck_more.is_empty(), "the same drive reads as the same roads: {d:?}");
}
