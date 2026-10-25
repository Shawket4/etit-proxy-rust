//! `Vehicle` — frontend-facing metadata about a tracked unit, plus its
//! latest known live state.
//!
//! Two sources feed one record. Tier A (`GetTransportersPage`, daily) owns the
//! metadata — id, codename, plate, speed limit, `extra`. The live columns —
//! `online`, `speed`, `status`, `status_label`, `last_location_at`, `lat`,
//! `lng` — come from whichever source saw the vehicle most recently, which in
//! practice is Tier B (`GetTransportersSpeedStatus`, every minute) via
//! [`Vehicle::apply_live`]. Without that, `GET /api/v1/vehicles` and the
//! SQLite `vehicles` table served a day-old Tier A snapshot as if it were
//! current.

use serde::{Deserialize, Serialize};

use super::live::LiveVehicleStatus;
use super::time::Timestamp;

/// ETIT status code for "Offline" (see [`super::status_label`]).
pub const STATUS_OFFLINE: i32 = 12;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vehicle {
    pub id: String,
    pub codename: String,
    pub plate: String,
    pub speed_limit: u32,
    pub online: bool,
    pub speed: u32,
    pub status: i32,
    pub status_label: String,
    pub last_location_at: Option<Timestamp>,
    /// Last known position — from Tier B; Tier A does not carry one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub lng: Option<f64>,

    #[serde(skip_serializing_if = "serde_json::Map::is_empty", default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Vehicle {
    /// Fold one Tier B reading into the live columns. A reading older than
    /// what the record already holds is ignored, so a late or replayed
    /// response can never move a vehicle back in time — unless what the
    /// record holds is in the future (`now` + slack), which no real fix can
    /// be and which must not freeze the record. Metadata is untouched.
    /// Returns whether anything was applied.
    pub fn apply_live(&mut self, live: &LiveVehicleStatus, now: Timestamp) -> bool {
        if let (Some(have), Some(new)) = (self.last_location_at, live.timestamp) {
            if new < have && !is_future(have, now) {
                return false;
            }
        }
        self.speed = live.speed.max(0) as u32;
        self.status = live.status;
        self.status_label = live.status_label.clone();
        self.online = live.status != STATUS_OFFLINE;
        if live.timestamp.is_some() {
            self.last_location_at = live.timestamp;
        }
        // 0,0 is ETIT's "no fix", not a point in the Gulf of Guinea.
        if live.lat != 0.0 || live.lng != 0.0 {
            self.lat = Some(live.lat);
            self.lng = Some(live.lng);
        }
        true
    }

    /// Keep `prior`'s live columns over this (freshly fetched Tier A) record's
    /// when `prior` saw the vehicle more recently — so the daily metadata
    /// refresh cannot overwrite a minute-old Tier B reading with its own
    /// older one.
    pub fn keep_newer_live_from(&mut self, prior: &Vehicle, now: Timestamp) {
        let prior_is_newer = match (prior.last_location_at, self.last_location_at) {
            (Some(p), Some(s)) => p > s || (is_future(s, now) && !is_future(p, now)),
            (Some(_), None) => true,
            _ => false,
        };
        if prior_is_newer {
            self.online = prior.online;
            self.speed = prior.speed;
            self.status = prior.status;
            self.status_label = prior.status_label.clone();
            self.last_location_at = prior.last_location_at;
        }
        // Tier A has no position, so a known one always survives.
        if self.lat.is_none() && self.lng.is_none() {
            self.lat = prior.lat;
            self.lng = prior.lng;
        }
    }

    /// Best-effort plate extraction from the upstream's "CODE/PLATE" codename.
    pub fn split_codename(codename: &str) -> (&str, &str) {
        match codename.split_once('/') {
            Some((code, plate)) => (code.trim(), plate.trim()),
            None => (codename.trim(), ""),
        }
    }
}

/// A fix this far past "now" is a misread zone, not a real reading.
fn is_future(t: Timestamp, now: Timestamp) -> bool {
    t > now + chrono::Duration::minutes(10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn utc(s: &str) -> Timestamp {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// 2026-10-25 10:35 Cairo — when the owner looked.
    fn now() -> Timestamp {
        utc("2026-10-25T07:35:00Z")
    }

    fn tier_a(last: Option<&str>) -> Vehicle {
        Vehicle {
            id: "v1".into(),
            codename: "OL/8567".into(),
            plate: "8567".into(),
            speed_limit: 80,
            online: true,
            speed: 0,
            status: 3,
            status_label: "Stopped (Ignition Off)".into(),
            last_location_at: last.map(utc),
            lat: None,
            lng: None,
            extra: Default::default(),
        }
    }

    fn live(ts: Option<&str>, status: i32, speed: i32) -> LiveVehicleStatus {
        LiveVehicleStatus {
            id: "v1".into(),
            plate: None,
            speed,
            status,
            status_label: super::super::status_label(status).into(),
            lat: 26.84,
            lng: 33.92,
            timestamp: ts.map(utc),
            event: None,
            sensor_data: None,
        }
    }

    #[test]
    fn newer_live_reading_replaces_the_live_columns() {
        let mut v = tier_a(Some("2026-10-25T06:48:43Z"));
        assert!(v.apply_live(&live(Some("2026-10-25T07:18:00Z"), 1, 86), now()));
        assert_eq!(v.last_location_at, Some(utc("2026-10-25T07:18:00Z")));
        assert_eq!((v.speed, v.status, v.online), (86, 1, true));
        assert_eq!(v.status_label, "Moving");
        assert_eq!((v.lat, v.lng), (Some(26.84), Some(33.92)));
        // Metadata is Tier A's alone.
        assert_eq!((v.plate.as_str(), v.speed_limit), ("8567", 80));
    }

    #[test]
    fn older_live_reading_is_ignored() {
        let mut v = tier_a(Some("2026-10-25T07:18:00Z"));
        assert!(!v.apply_live(&live(Some("2026-10-25T07:00:00Z"), 1, 50), now()));
        assert_eq!(v.status, 3);
        assert_eq!(v.last_location_at, Some(utc("2026-10-25T07:18:00Z")));
    }

    #[test]
    fn offline_status_clears_online_and_untimed_reading_keeps_the_last_fix() {
        let mut v = tier_a(Some("2026-10-25T06:48:43Z"));
        let mut r = live(None, STATUS_OFFLINE, 0);
        r.lat = 0.0;
        r.lng = 0.0;
        assert!(v.apply_live(&r, now()));
        assert!(!v.online);
        assert_eq!(v.last_location_at, Some(utc("2026-10-25T06:48:43Z")));
        assert_eq!(v.lat, None, "0,0 is no fix");
    }

    #[test]
    fn tier_a_refresh_keeps_a_newer_tier_b_reading() {
        let mut prior = tier_a(Some("2026-10-25T06:48:43Z"));
        prior.apply_live(&live(Some("2026-10-25T07:18:00Z"), 1, 86), now());

        // Tier A comes back with its own (older) view of the same vehicle.
        let mut fetched = tier_a(Some("2026-10-25T07:10:00Z"));
        fetched.speed_limit = 90;
        fetched.keep_newer_live_from(&prior, now());
        assert_eq!(fetched.last_location_at, Some(utc("2026-10-25T07:18:00Z")));
        assert_eq!((fetched.speed, fetched.status), (86, 1));
        assert_eq!(fetched.speed_limit, 90, "metadata comes from the fetch");
        assert_eq!(fetched.lat, Some(26.84));

        // A Tier A reading newer than anything seen wins.
        let mut fresher = tier_a(Some("2026-10-25T07:30:00Z"));
        fresher.keep_newer_live_from(&prior, now());
        assert_eq!(fresher.last_location_at, Some(utc("2026-10-25T07:30:00Z")));
        assert_eq!(fresher.status, 3);
    }

    /// If Tier A's zone were ever misconfigured the other way, its fixes would
    /// land hours in the future; that must not lock Tier B out.
    #[test]
    fn a_future_fix_does_not_block_live_readings() {
        let mut v = tier_a(Some("2026-10-25T10:18:00Z")); // 3h ahead of now()
        assert!(v.apply_live(&live(Some("2026-10-25T07:18:00Z"), 1, 86), now()));
        assert_eq!(v.last_location_at, Some(utc("2026-10-25T07:18:00Z")));

        let prior = v.clone();
        let mut fetched = tier_a(Some("2026-10-25T10:20:00Z"));
        fetched.keep_newer_live_from(&prior, now());
        assert_eq!(fetched.last_location_at, Some(utc("2026-10-25T07:18:00Z")));
    }
}
