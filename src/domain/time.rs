//! Time handling.
//!
//! Internally and at the API boundary we use **UTC, ISO-8601 / RFC3339** — a
//! shape browsers and Java(Script) parse trivially. The legacy ETIT system
//! emits localized strings in several formats; [`parse_etit_local`] tries
//! them in order and converts to UTC.
//!
//! `Timestamp` is a thin alias so the rest of the codebase doesn't import
//! chrono directly.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

pub type Timestamp = DateTime<Utc>;

pub fn utc_now() -> Timestamp {
    Utc::now()
}

/// The clock-time formats ETIT has been seen to emit, most → least specific.
/// The first one that parses wins.
const FORMATS: &[&str] = &[
    "%d/%m/%Y %I:%M:%S %p",       // 26/04/2026 03:13:37 PM
    "%d/%m/%Y %H:%M:%S",          // 26/04/2026 15:13:37
    "%-m/%-d/%Y %H:%M:%S",        // 4/26/2026 15:13:37
    "%-m/%-d/%Y %I:%M:%S %p",     // 4/26/2026 03:13:37 PM
    "%d %B %Y %H:%M:%S",          // 26 April 2026 16:28:00
    "%d %B %Y %H:%M",             // 26 April 2026 16:28
    "%d %B %Y %I:%M %p",          // 26 April 2026 04:28 PM
    "%Y-%m-%dT%H:%M:%S%.f",       // 2026-04-26T16:28:00(.123)
    "%Y-%m-%d %H:%M:%S%.f",       // 2026-04-26 16:28:00(.123)
];

/// What a raw upstream string turned out to be: a wall-clock time with no
/// zone (the usual case), or an instant that carried its own offset.
enum Parsed {
    Naive(NaiveDateTime),
    Instant(Timestamp),
}

fn parse_raw(raw: &str) -> Option<Parsed> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    for fmt in FORMATS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(Parsed::Naive(naive));
        }
    }
    // Fallback: RFC3339 (with offset embedded) — the offset is authoritative,
    // whatever zone the caller assumed.
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| Parsed::Instant(dt.with_timezone(&Utc)))
}

/// Parse one of the upstream's many timestamp formats as a *wall-clock* time
/// in `tz`, then convert to UTC. Returns `None` if no format matches.
///
/// Which zone a field is in is a property of the ETIT endpoint, not of the
/// string, so every caller names it:
///
/// | field                                        | zone            |
/// |----------------------------------------------|-----------------|
/// | Tier B `GetTransportersSpeedStatus` times    | `TIMEZONE` (Cairo wall clock) |
/// | history `d`, stops `from`/`to`, sensors      | `TIMEZONE` (Cairo wall clock) |
/// | Tier A `GetTransportersPage.LastLocationDate`| **UTC** — `ETIT_LAST_LOCATION_TZ` |
/// | API `from`/`to` query strings                | `TIMEZONE`      |
///
/// Pass `chrono_tz::UTC` for a field ETIT sends in UTC.
///
/// A wall time inside the autumn repeated hour resolves to its *first*
/// occurrence; use [`parse_etit_local_after`] for an ordered series.
pub fn parse_etit_local(raw: &str, tz: Tz) -> Option<Timestamp> {
    parse_etit_local_after(raw, tz, None)
}

/// [`parse_etit_local`] for one element of a chronological series (history
/// points, a stop's `to` after its `from`). In the autumn repeated hour the
/// same wall time names two instants an hour apart; picking the first one
/// for both passes would fold the second hour back onto the first and send
/// the series backwards. Given the previous instant of the series, an
/// ambiguous wall time resolves to the later occurrence once `prev` is past
/// the switch, or when the earlier one would land well (over 30 min) before
/// `prev`.
pub fn parse_etit_local_after(raw: &str, tz: Tz, prev: Option<Timestamp>) -> Option<Timestamp> {
    match parse_raw(raw)? {
        Parsed::Instant(t) => Some(t),
        Parsed::Naive(naive) => {
            use chrono::offset::LocalResult::*;
            match tz.from_local_datetime(&naive) {
                Ambiguous(early, late) => {
                    use chrono::Offset;
                    let late_offset = late.offset().fix();
                    let (early, late) = (early.with_timezone(&Utc), late.with_timezone(&Utc));
                    let second_pass = match prev {
                        // The series is already past the switch: everything
                        // ambiguous from here on is the second occurrence.
                        Some(p) if p > early && p.with_timezone(&tz).offset().fix() == late_offset => {
                            true
                        }
                        // The series just jumped back across the switch. Half
                        // an hour of slack so a point or two out of order
                        // inside the first pass is not thrown an hour forward.
                        Some(p) => p - early > chrono::Duration::minutes(30),
                        Option::None => false,
                    };
                    Some(if second_pass { late } else { early })
                }
                _ => naive_in_tz_to_utc(naive, tz, Pick::Earliest),
            }
        }
    }
}

/// Format a UTC instant for the upstream's `M/D/YYYY HH:MM:SS` history
/// query — which it expects in *its own* local time.
pub fn etit_format_for_history(t: Timestamp, tz: Tz) -> String {
    t.with_timezone(&tz)
        .format("%-m/%-d/%Y %H:%M:%S")
        .to_string()
}

/// Today's date in the configured timezone.
pub fn today_local(tz: Tz) -> NaiveDate {
    Utc::now().with_timezone(&tz).date_naive()
}

/// Start-of-day (00:00:00 local) → UTC. On a spring-forward day where
/// midnight itself does not exist (Egypt: last Friday of April), this is the
/// first instant that does.
pub fn start_of_day_utc(date: NaiveDate, tz: Tz) -> Timestamp {
    naive_in_tz_to_utc(
        date.and_hms_opt(0, 0, 0).expect("valid 00:00:00"),
        tz,
        Pick::Earliest,
    )
    .unwrap_or_else(utc_now)
}

/// End-of-day (23:59:59 local) → UTC. On a fall-back day whose last hour
/// repeats (Egypt: last Thursday of October, 24:00 → 23:00) this is the
/// *second* 23:59:59, so the day's window reaches the next day's midnight
/// instead of dropping the repeated hour on the floor.
pub fn end_of_day_utc(date: NaiveDate, tz: Tz) -> Timestamp {
    naive_in_tz_to_utc(
        date.and_hms_opt(23, 59, 59).expect("valid 23:59:59"),
        tz,
        Pick::Latest,
    )
    .unwrap_or_else(utc_now)
}

#[derive(Clone, Copy)]
enum Pick {
    Earliest,
    Latest,
}

/// Helper: a naive local datetime → UTC. An ambiguous wall time (repeated
/// hour) resolves per `pick`; one in a DST gap is nudged forward an hour.
fn naive_in_tz_to_utc(naive: NaiveDateTime, tz: Tz, pick: Pick) -> Option<Timestamp> {
    use chrono::offset::LocalResult::*;
    match tz.from_local_datetime(&naive) {
        Single(local) => Some(local.with_timezone(&Utc)),
        Ambiguous(early, late) => Some(match pick {
            Pick::Earliest => early.with_timezone(&Utc),
            Pick::Latest => late.with_timezone(&Utc),
        }),
        None => {
            // DST gap; nudge forward by one hour.
            let bumped = naive.checked_add_signed(chrono::Duration::hours(1))?;
            tz.from_local_datetime(&bumped)
                .earliest()
                .map(|d| d.with_timezone(&Utc))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_few_formats() {
        let cairo = chrono_tz::Africa::Cairo;
        for s in [
            "26/04/2026 16:28:00",
            "26/04/2026 04:28:00 PM",
            "26 April 2026 16:28",
            "4/26/2026 16:28:00",
        ] {
            assert!(
                parse_etit_local(s, cairo).is_some(),
                "failed to parse `{s}`",
            );
        }
    }

    #[test]
    fn empty_is_none() {
        assert!(parse_etit_local("", chrono_tz::UTC).is_none());
        assert!(parse_etit_local("   ", chrono_tz::UTC).is_none());
    }

    #[test]
    fn round_trips_history_format() {
        let cairo = chrono_tz::Africa::Cairo;
        let t = parse_etit_local("26/04/2026 16:28:00", cairo).unwrap();
        let formatted = etit_format_for_history(t, cairo);
        assert_eq!(formatted, "4/26/2026 16:28:00");
    }

    fn utc(s: &str) -> Timestamp {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const CAIRO: Tz = chrono_tz::Africa::Cairo;

    // ---- Field formats ---------------------------------------------------

    /// Tier B (`GetTransportersSpeedStatus`) sends Cairo wall clock. On
    /// 2026-10-25 Cairo is still EEST (UTC+3): 10:18 local is 07:18Z.
    #[test]
    fn tier_b_time_is_cairo_wall_clock() {
        assert_eq!(
            parse_etit_local("25 October 2026 10:18", CAIRO),
            Some(utc("2026-10-25T07:18:00Z")),
        );
    }

    /// Tier A (`GetTransportersPage.LastLocationDate`) is UTC. Reading it as
    /// Cairo wall clock is what put every vehicle's last_location_at three
    /// hours in the past on 2026-10-25 (8567: 06:48 sent → 03:48Z stored,
    /// true instant 06:48Z = 09:48 Cairo).
    #[test]
    fn tier_a_last_location_is_utc() {
        for raw in [
            "25/10/2026 06:48:43",
            "25/10/2026 06:48:43 AM",
            "10/25/2026 06:48:43",
            "2026-10-25T06:48:43",
            "2026-10-25 06:48:43",
            "2026-10-25T06:48:43.000",
        ] {
            assert_eq!(
                parse_etit_local(raw, chrono_tz::UTC),
                Some(utc("2026-10-25T06:48:43Z")),
                "`{raw}`",
            );
        }
        // The bug, pinned: the same string read as Cairo is exactly 3h early.
        let wrong = parse_etit_local("25/10/2026 06:48:43", CAIRO).unwrap();
        assert_eq!(wrong, utc("2026-10-25T03:48:43Z"));
    }

    /// An explicit offset always wins over the assumed zone.
    #[test]
    fn rfc3339_offset_is_authoritative() {
        for tz in [CAIRO, chrono_tz::UTC] {
            assert_eq!(
                parse_etit_local("2026-10-25T09:48:43+03:00", tz),
                Some(utc("2026-10-25T06:48:43Z")),
            );
        }
    }

    /// History `d` / stops `from`,`to` / sensors are Cairo wall clock, the
    /// same zone the history query is sent in.
    #[test]
    fn history_formats_are_cairo_wall_clock() {
        assert_eq!(
            parse_etit_local("25/04/2026 01:06:34", CAIRO),
            Some(utc("2026-04-24T22:06:34Z")), // EEST, the day after spring-forward
        );
        assert_eq!(
            parse_etit_local("1/15/2026 13:00:00", CAIRO),
            Some(utc("2026-01-15T11:00:00Z")), // EET in winter
        );
    }

    // ---- Egyptian DST (chrono-tz must know the 2023+ rule) ---------------

    fn cairo_offset_hours(t: Timestamp) -> i32 {
        use chrono::Offset;
        t.with_timezone(&CAIRO).offset().fix().local_minus_utc() / 3600
    }

    #[test]
    fn egypt_summer_time_2026_boundaries() {
        // Spring forward: Fri 2026-04-24 00:00 EET → 01:00 EEST (22:00Z Thu).
        assert_eq!(cairo_offset_hours(utc("2026-04-23T21:59:59Z")), 2);
        assert_eq!(cairo_offset_hours(utc("2026-04-23T22:00:00Z")), 3);
        // The owner's observation day is still summer time.
        assert_eq!(cairo_offset_hours(utc("2026-10-25T07:35:00Z")), 3);
        // Fall back: Thu 2026-10-29 24:00 EEST → 23:00 EET (21:00Z).
        assert_eq!(cairo_offset_hours(utc("2026-10-29T20:59:59Z")), 3);
        assert_eq!(cairo_offset_hours(utc("2026-10-29T21:00:00Z")), 2);
        assert_eq!(cairo_offset_hours(utc("2026-11-01T12:00:00Z")), 2);
    }

    #[test]
    fn day_bounds_tile_the_timeline_across_dst() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        // Ordinary summer and winter days.
        assert_eq!(start_of_day_utc(d("2026-10-25"), CAIRO), utc("2026-10-24T21:00:00Z"));
        assert_eq!(end_of_day_utc(d("2026-10-25"), CAIRO), utc("2026-10-25T20:59:59Z"));
        assert_eq!(start_of_day_utc(d("2026-11-01"), CAIRO), utc("2026-10-31T22:00:00Z"));

        // Spring-forward day: local midnight does not exist; the day starts
        // at 01:00 EEST, one second after the previous day's end.
        assert_eq!(end_of_day_utc(d("2026-04-23"), CAIRO), utc("2026-04-23T21:59:59Z"));
        assert_eq!(start_of_day_utc(d("2026-04-24"), CAIRO), utc("2026-04-23T22:00:00Z"));

        // Fall-back day: 23:00–23:59 repeats; the day must end at the second
        // 23:59:59 so the next day's midnight follows it with no gap.
        assert_eq!(end_of_day_utc(d("2026-10-29"), CAIRO), utc("2026-10-29T21:59:59Z"));
        assert_eq!(start_of_day_utc(d("2026-10-30"), CAIRO), utc("2026-10-29T22:00:00Z"));

        // Every day: next start is exactly one second after this end.
        let mut day = d("2026-01-01");
        while day < d("2027-01-01") {
            let next = day.succ_opt().unwrap();
            assert_eq!(
                start_of_day_utc(next, CAIRO) - end_of_day_utc(day, CAIRO),
                chrono::Duration::seconds(1),
                "{day} → {next}",
            );
            day = next;
        }
    }

    #[test]
    fn repeated_hour_resolves_forward_in_a_series() {
        // 23:30 on 2026-10-29 happens twice: 20:30Z (EEST) then 21:30Z (EET).
        let first = parse_etit_local("29/10/2026 23:30:00", CAIRO).unwrap();
        assert_eq!(first, utc("2026-10-29T20:30:00Z"));
        // After the last EEST point (23:59 → 20:59Z) the clock falls back;
        // 23:05 and 23:30 now mean the EET pass.
        let prev = utc("2026-10-29T20:59:00Z");
        assert_eq!(
            parse_etit_local_after("29/10/2026 23:05:00", CAIRO, Some(prev)),
            Some(utc("2026-10-29T21:05:00Z")),
        );
        assert_eq!(
            parse_etit_local_after("29/10/2026 23:30:00", CAIRO, Some(utc("2026-10-29T21:05:00Z"))),
            Some(utc("2026-10-29T21:30:00Z")),
        );
        // Jitter inside the first pass (a point a minute older than the one
        // before it) is not a clock change.
        assert_eq!(
            parse_etit_local_after("29/10/2026 23:30:00", CAIRO, Some(utc("2026-10-29T20:31:00Z"))),
            Some(first),
        );
        // Without a later predecessor the first occurrence stands.
        assert_eq!(
            parse_etit_local_after("29/10/2026 23:30:00", CAIRO, Some(utc("2026-10-29T20:00:00Z"))),
            Some(first),
        );
        // Unambiguous times ignore `prev` entirely.
        assert_eq!(
            parse_etit_local_after("25/10/2026 10:18:00", CAIRO, Some(utc("2030-01-01T00:00:00Z"))),
            Some(utc("2026-10-25T07:18:00Z")),
        );
    }

    #[test]
    fn spring_gap_wall_time_is_nudged_forward() {
        // 00:30 on 2026-04-24 never happened in Cairo; read as 01:30 EEST.
        assert_eq!(
            parse_etit_local("24/04/2026 00:30:00", CAIRO),
            Some(utc("2026-04-23T22:30:00Z")),
        );
    }

    #[test]
    fn cairo_day_bucketing_at_midnight() {
        // 23:59:59 and 00:00:00 Cairo fall on different local days even though
        // both are the same UTC date (summer: 20:59:59Z / 21:00:00Z).
        let a = parse_etit_local("25/10/2026 23:59:59", CAIRO).unwrap();
        let b = parse_etit_local("26/10/2026 00:00:00", CAIRO).unwrap();
        assert_eq!(a.date_naive(), b.date_naive());
        assert_eq!(a.with_timezone(&CAIRO).date_naive().to_string(), "2026-10-25");
        assert_eq!(b.with_timezone(&CAIRO).date_naive().to_string(), "2026-10-26");
    }
}
