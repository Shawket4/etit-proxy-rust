//! Roads closed to the fleet's trucks, as built into the Valhalla tiles.
//!
//! `valhalla/build.sh` writes each ban in `valhalla/time-bans.json` into the
//! map: `hgv=no` for a road closed at all hours, `hgv:conditional` for one
//! closed only at some hours. Valhalla's routing therefore already avoids a
//! banned road whenever it is closed. The build also writes
//! `apex-time-bans.json`: every way each ban caught. This module reads that
//! file so the trip audit can answer the other half: did a truck actually
//! DRIVE a banned road while it was closed? The map-matched edges carry OSM
//! way ids; a match while the ban is in force is a violation.
//!
//! Hours are local wall-clock time (the org timezone, Africa/Cairo).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{NaiveTime, TimeZone};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
struct BanFile {
    bans: Vec<BanEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct BanEntry {
    id: String,
    label: String,
    #[serde(default)]
    label_ar: String,
    /// Closed at every hour. Exclusive with banned_from/banned_to.
    #[serde(default)]
    always: bool,
    #[serde(default)]
    banned_from: Option<String>,
    #[serde(default)]
    banned_to: Option<String>,
    way_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ban {
    pub id: String,
    pub label: String,
    pub label_ar: String,
    /// The closed hours, [from, to) local; None when closed at every hour.
    pub window: Option<(NaiveTime, NaiveTime)>,
}

impl Ban {
    pub fn always(&self) -> bool {
        self.window.is_none()
    }

    /// In force at `t`: always, or inside [from, to), wrapping past midnight
    /// when from > to.
    pub fn covers(&self, t: NaiveTime) -> bool {
        match self.window {
            None => true,
            Some((from, to)) if from <= to => t >= from && t < to,
            Some((from, to)) => t >= from || t < to,
        }
    }

    /// "07:00-20:00"; None for a road closed at every hour.
    pub fn hours(&self) -> Option<String> {
        self.window
            .map(|(from, to)| format!("{}-{}", from.format("%H:%M"), to.format("%H:%M")))
    }
}

/// Way id → the bans on it. Empty when no file is configured or found: the
/// audit then simply raises no ban flags.
#[derive(Debug, Clone, Default)]
pub struct TimeBans {
    by_way: HashMap<i64, Vec<usize>>,
    bans: Vec<Ban>,
}

impl TimeBans {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let file: BanFile = serde_json::from_str(raw).context("parsing time bans")?;
        let mut out = TimeBans::default();
        for entry in file.bans {
            let parse = |s: &str| {
                NaiveTime::parse_from_str(s, "%H:%M")
                    .with_context(|| format!("ban {}: bad time {s:?}", entry.id))
            };
            let window = match (entry.always, &entry.banned_from, &entry.banned_to) {
                (true, None, None) => None,
                (false, Some(from), Some(to)) => Some((parse(from)?, parse(to)?)),
                _ => anyhow::bail!(
                    "ban {}: give either \"always\": true or both banned_from and banned_to",
                    entry.id
                ),
            };
            let ban = Ban {
                window,
                id: entry.id,
                label: entry.label,
                label_ar: entry.label_ar,
            };
            let idx = out.bans.len();
            for way in entry.way_ids {
                out.by_way.entry(way).or_default().push(idx);
            }
            out.bans.push(ban);
        }
        Ok(out)
    }

    /// A local time inside the first timed ban's window: two hours after it
    /// opens, or its midpoint when it is shorter than four hours. A route
    /// planned to leave then avoids the time-banned roads. None without timed
    /// bans: roads closed at every hour are avoided whatever the time.
    pub fn reference_time(&self) -> Option<NaiveTime> {
        let (from, to) = self.bans.iter().find_map(|b| b.window)?;
        let len = if from <= to {
            to - from
        } else {
            (to - from) + chrono::Duration::hours(24)
        };
        let offset = if len >= chrono::Duration::hours(4) { chrono::Duration::hours(2) } else { len / 2 };
        Some(from + offset)
    }

    pub fn is_empty(&self) -> bool {
        self.bans.is_empty()
    }

    pub fn way_count(&self) -> usize {
        self.by_way.len()
    }

    /// The ban in force on `way_id` at `epoch` (local time in `tz`), if any.
    pub fn violated<Tz: TimeZone>(&self, way_id: i64, epoch: i64, tz: &Tz) -> Option<&Ban> {
        let idxs = self.by_way.get(&way_id)?;
        let local = tz.timestamp_opt(epoch, 0).single()?.naive_local().time();
        idxs.iter().map(|&i| &self.bans[i]).find(|b| b.covers(local))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"{"bans":[
      {"id":"ahmed-hamdi-tunnel","label":"Ahmed Hamdi tunnel","label_ar":"نفق الشهيد أحمد حمدي",
       "always":true,"way_ids":[300,301],"ways":[]},
      {"id":"day-only","label":"Day ban","label_ar":"حظر نهاري",
       "banned_from":"07:00","banned_to":"20:00","way_ids":[100,101],"ways":[]},
      {"id":"night-only","label":"Night ban","banned_from":"22:00","banned_to":"05:00","way_ids":[200]}
    ]}"#;

    fn cairo(h: u32, m: u32) -> i64 {
        chrono_tz::Africa::Cairo
            .with_ymd_and_hms(2026, 11, 27, h, m, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    #[test]
    fn a_day_ban_applies_by_day_only() {
        let b = TimeBans::parse(FILE).unwrap();
        let tz = chrono_tz::Africa::Cairo;
        assert_eq!(b.violated(100, cairo(13, 0), &tz).map(|x| x.id.as_str()), Some("day-only"));
        assert!(b.violated(101, cairo(7, 0), &tz).is_some(), "the start of the window is banned");
        assert!(b.violated(101, cairo(20, 0), &tz).is_none(), "the end of the window is not");
        assert!(b.violated(100, cairo(23, 30), &tz).is_none());
        assert!(b.violated(999, cairo(13, 0), &tz).is_none(), "other roads are never banned");
    }

    #[test]
    fn a_road_closed_always_is_banned_at_every_hour() {
        let b = TimeBans::parse(FILE).unwrap();
        let tz = chrono_tz::Africa::Cairo;
        for (h, m) in [(0, 0), (3, 30), (7, 0), (13, 0), (20, 0), (23, 59)] {
            let ban = b.violated(300, cairo(h, m), &tz).expect("closed at every hour");
            assert_eq!(ban.id, "ahmed-hamdi-tunnel");
            assert!(ban.always());
            assert_eq!(ban.hours(), None);
        }
        assert_eq!(b.bans[1].hours().as_deref(), Some("07:00-20:00"));
    }

    #[test]
    fn a_window_can_wrap_past_midnight() {
        let b = TimeBans::parse(FILE).unwrap();
        let tz = chrono_tz::Africa::Cairo;
        assert!(b.violated(200, cairo(23, 0), &tz).is_some());
        assert!(b.violated(200, cairo(2, 0), &tz).is_some());
        assert!(b.violated(200, cairo(12, 0), &tz).is_none());
    }

    #[test]
    fn hours_are_cairo_local_not_utc() {
        // 06:30 UTC is 08:30 in Cairo (UTC+2 in November): inside the ban.
        let b = TimeBans::parse(FILE).unwrap();
        let epoch = chrono::Utc.with_ymd_and_hms(2026, 11, 27, 6, 30, 0).unwrap().timestamp();
        assert!(b.violated(100, epoch, &chrono_tz::Africa::Cairo).is_some());
        assert!(b.violated(100, epoch, &chrono::Utc).is_none());
    }

    #[test]
    fn the_reference_time_sits_inside_the_first_timed_window() {
        // The always-closed ban comes first in FILE and has no window to sit in.
        let b = TimeBans::parse(FILE).unwrap();
        let t = b.reference_time().unwrap();
        assert_eq!(t, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        assert!(b.bans[1].covers(t));
        assert_eq!(TimeBans::default().reference_time(), None);
        let only_always = r#"{"bans":[{"id":"x","label":"x","always":true,"way_ids":[1]}]}"#;
        assert_eq!(TimeBans::parse(only_always).unwrap().reference_time(), None);
    }

    #[test]
    fn a_bad_time_is_an_error_not_a_silent_open_road() {
        let raw = r#"{"bans":[{"id":"x","label":"x","banned_from":"7am","banned_to":"20:00","way_ids":[1]}]}"#;
        assert!(TimeBans::parse(raw).is_err());
    }

    #[test]
    fn a_ban_needs_either_always_or_both_hours() {
        for entry in [
            r#""always":true,"banned_from":"07:00","banned_to":"20:00""#,
            r#""banned_from":"07:00""#,
            r#""always":false"#,
        ] {
            let raw = format!(r#"{{"bans":[{{"id":"x","label":"x",{entry},"way_ids":[1]}}]}}"#);
            assert!(TimeBans::parse(&raw).is_err(), "{entry} must be refused");
        }
    }
}
