//! Did a truck drive a road closed to it, while it was closed?
//!
//! Input is a leg's map-matched edges (OSM way ids, with the times of the
//! GPS fixes matched onto each) and the bans built into the tiles. Output is
//! one episode per continuous stretch on a banned road inside its hours,
//! which the scan raises as a `banned_road` flag on the leg.

use chrono::TimeZone;
use serde::Serialize;

use crate::routing::valhalla::MatchedEdge;
use crate::routing::TimeBans;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BanUse {
    pub ban_id: String,
    pub label: String,
    pub label_ar: String,
    /// Closed at every hour.
    pub always: bool,
    /// "07:00-20:00", local; None when closed at every hour.
    pub hours: Option<String>,
    /// When the truck was on the banned road (epoch seconds).
    pub from_ts: i64,
    pub to_ts: i64,
    pub km: f64,
    /// Road names driven during the episode, deduplicated, in order.
    pub roads: Vec<String>,
}

/// Two stretches of the same ban closer than this are one drive: a short
/// connector or slip road between them is not leaving the corridor.
const MERGE_GAP_SECS: i64 = 300;

/// Episodes of banned-road use, in driving order. An edge with no fix of its
/// own (the matcher interpolated across it) takes the time of the fix before
/// it, which is how far apart ETIT fixes are anyway (about a minute).
pub fn banned_use<Tz: TimeZone>(edges: &[MatchedEdge], bans: &TimeBans, tz: &Tz) -> Vec<BanUse> {
    let mut out: Vec<BanUse> = Vec::new();
    let mut last_time: Option<i64> = None;
    // Index into `out` of the episode the previous edge extended, if any.
    let mut open: Option<usize> = None;

    for e in edges {
        let first = e.first_ts.or(last_time);
        let last = e.last_ts.or(first);
        if let Some(t) = e.last_ts.or(e.first_ts) {
            last_time = Some(t);
        }
        let (Some(first), Some(last)) = (first, last) else {
            open = None;
            continue;
        };
        let hit = bans
            .violated(e.way_id, first, tz)
            .or_else(|| bans.violated(e.way_id, last, tz));
        let Some(ban) = hit else {
            open = None;
            continue;
        };

        // Continue the episode the previous edge was on, or the last one of
        // this ban if the truck rejoined within MERGE_GAP_SECS.
        let target = open.or_else(|| {
            out.iter()
                .rposition(|u| u.ban_id == ban.id && first - u.to_ts <= MERGE_GAP_SECS)
                .filter(|&i| i + 1 == out.len())
        });
        match target.and_then(|i| out.get_mut(i)).filter(|u| u.ban_id == ban.id) {
            Some(u) => {
                u.to_ts = u.to_ts.max(last);
                u.km += e.length_km;
                for n in &e.names {
                    if !u.roads.contains(n) {
                        u.roads.push(n.clone());
                    }
                }
            }
            None => {
                out.push(BanUse {
                    ban_id: ban.id.clone(),
                    label: ban.label.clone(),
                    label_ar: ban.label_ar.clone(),
                    always: ban.always(),
                    hours: ban.hours(),
                    from_ts: first,
                    to_ts: last,
                    km: e.length_km,
                    roads: e.names.clone(),
                });
                open = Some(out.len() - 1);
            }
        }
    }
    for u in &mut out {
        u.km = (u.km * 100.0).round() / 100.0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A road closed 07:00-20:00 (ways 10-12) and a tunnel closed at every hour (20).
    const BANS: &str = r#"{"bans":[
        {"id":"day-ban","label":"Day-banned road","label_ar":"طريق محظور نهارا",
         "banned_from":"07:00","banned_to":"20:00","way_ids":[10,11,12]},
        {"id":"ahmed-hamdi-tunnel","label":"Ahmed Hamdi tunnel","label_ar":"نفق الشهيد أحمد حمدي",
         "always":true,"way_ids":[20]}]}"#;

    fn at(h: u32, m: u32) -> i64 {
        chrono_tz::Africa::Cairo.with_ymd_and_hms(2026, 11, 27, h, m, 0).single().unwrap().timestamp()
    }

    fn edge(way: i64, name: &str, km: f64, t: Option<i64>) -> MatchedEdge {
        MatchedEdge { way_id: way, names: vec![name.into()], length_km: km, first_ts: t, last_ts: t }
    }

    #[test]
    fn a_daytime_stretch_on_the_corridor_is_one_episode() {
        let bans = TimeBans::parse(BANS).unwrap();
        let edges = vec![
            edge(1, "الطريق الدائري", 3.0, Some(at(13, 0))),
            edge(10, "محور 26 يوليو", 1.2, Some(at(13, 4))),
            edge(11, "محور 26 يوليو", 0.8, None), // interpolated: takes 13:04
            edge(12, "محور 26 يوليو", 2.0, Some(at(13, 9))),
            edge(2, "شارع جامعة الدول", 1.0, Some(at(13, 12))),
        ];
        let uses = banned_use(&edges, &bans, &chrono_tz::Africa::Cairo);
        assert_eq!(uses.len(), 1);
        let u = &uses[0];
        assert_eq!((u.from_ts, u.to_ts), (at(13, 4), at(13, 9)));
        assert_eq!(u.km, 4.0);
        assert_eq!(u.roads, vec!["محور 26 يوليو".to_string()]);
        assert_eq!(u.hours.as_deref(), Some("07:00-20:00"));
        assert!(!u.always);
    }

    #[test]
    fn the_same_road_at_night_is_fine() {
        let bans = TimeBans::parse(BANS).unwrap();
        let edges = vec![edge(10, "محور 26 يوليو", 1.2, Some(at(22, 30))), edge(11, "محور 26 يوليو", 1.0, Some(at(22, 32)))];
        assert!(banned_use(&edges, &bans, &chrono_tz::Africa::Cairo).is_empty());
    }

    #[test]
    fn a_road_closed_at_every_hour_is_flagged_at_night_too() {
        let bans = TimeBans::parse(BANS).unwrap();
        let edges = vec![
            edge(3, "طريق نفق الشهيد احمد حمدى", 2.0, Some(at(2, 10))),
            edge(20, "نفق الشهيد احمد حمدى", 1.8, Some(at(2, 12))),
            edge(4, "طريق نفق الشهيد احمد حمدى", 2.0, Some(at(2, 14))),
        ];
        let uses = banned_use(&edges, &bans, &chrono_tz::Africa::Cairo);
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].ban_id, "ahmed-hamdi-tunnel");
        assert!(uses[0].always);
        assert_eq!(uses[0].hours, None);
        assert_eq!(uses[0].km, 1.8);
    }

    #[test]
    fn leaving_and_coming_back_is_two_episodes() {
        let bans = TimeBans::parse(BANS).unwrap();
        let edges = vec![
            edge(10, "a", 1.0, Some(at(9, 0))),
            edge(5, "other", 1.0, Some(at(9, 5))),
            edge(12, "a", 1.0, Some(at(9, 10))),
        ];
        assert_eq!(banned_use(&edges, &bans, &chrono_tz::Africa::Cairo).len(), 2);
    }

    #[test]
    fn a_short_connector_does_not_split_one_drive() {
        let bans = TimeBans::parse(BANS).unwrap();
        let edges = vec![
            edge(10, "محور 26 يوليو", 5.0, Some(at(13, 0))),
            edge(5, "slip road", 0.2, Some(at(13, 2))),
            edge(12, "محور 26 يوليو الجانبى", 0.3, Some(at(13, 3))),
        ];
        let uses = banned_use(&edges, &bans, &chrono_tz::Africa::Cairo);
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].km, 5.3);
        assert_eq!(uses[0].to_ts, at(13, 3));
        assert_eq!(uses[0].roads.len(), 2);
    }

    #[test]
    fn entering_just_before_the_ban_ends_still_counts() {
        // On the corridor 19:58 → 20:03: the first fix is inside the window.
        let bans = TimeBans::parse(BANS).unwrap();
        let edges = vec![MatchedEdge {
            way_id: 10,
            names: vec!["a".into()],
            length_km: 3.0,
            first_ts: Some(at(19, 58)),
            last_ts: Some(at(20, 3)),
        }];
        assert_eq!(banned_use(&edges, &bans, &chrono_tz::Africa::Cairo).len(), 1);
    }

    #[test]
    fn no_bans_loaded_means_no_flags() {
        let edges = vec![edge(10, "a", 1.0, Some(at(13, 0)))];
        assert!(banned_use(&edges, &TimeBans::default(), &chrono_tz::Africa::Cairo).is_empty());
    }
}
