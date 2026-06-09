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

/// Parse one of the upstream's many timestamp formats as a *local* clock-time
/// in `tz`, then convert to UTC. Returns `None` if no format matches.
pub fn parse_etit_local(raw: &str, tz: Tz) -> Option<Timestamp> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }

    // Ordered from most → least specific. Stop on the first match.
    const FORMATS: &[&str] = &[
        "%d/%m/%Y %I:%M:%S %p",       // 26/04/2026 03:13:37 PM
        "%d/%m/%Y %H:%M:%S",          // 26/04/2026 15:13:37
        "%-m/%-d/%Y %H:%M:%S",        // 4/26/2026 15:13:37
        "%-m/%-d/%Y %I:%M:%S %p",     // 4/26/2026 03:13:37 PM
        "%d %B %Y %H:%M:%S",          // 26 April 2026 16:28:00
        "%d %B %Y %H:%M",             // 26 April 2026 16:28
        "%d %B %Y %I:%M %p",          // 26 April 2026 04:28 PM
        "%Y-%m-%dT%H:%M:%S",          // 2026-04-26T16:28:00
        "%Y-%m-%d %H:%M:%S",          // 2026-04-26 16:28:00
    ];

    for fmt in FORMATS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return naive_in_tz_to_utc(naive, tz);
        }
    }

    // Fallback: RFC3339 (with offset embedded).
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }

    None
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

/// Start-of-day (00:00:00 local) → UTC.
pub fn start_of_day_utc(date: NaiveDate, tz: Tz) -> Timestamp {
    naive_in_tz_to_utc(
        date.and_hms_opt(0, 0, 0).expect("valid 00:00:00"),
        tz,
    )
    .unwrap_or_else(utc_now)
}

/// End-of-day (23:59:59 local) → UTC.
pub fn end_of_day_utc(date: NaiveDate, tz: Tz) -> Timestamp {
    naive_in_tz_to_utc(
        date.and_hms_opt(23, 59, 59).expect("valid 23:59:59"),
        tz,
    )
    .unwrap_or_else(utc_now)
}

/// Helper: a naive local datetime → UTC, picking the earliest valid
/// instant if the local time falls in a DST gap, and choosing the first
/// of an ambiguous pair otherwise.
fn naive_in_tz_to_utc(naive: NaiveDateTime, tz: Tz) -> Option<Timestamp> {
    use chrono::offset::LocalResult::*;
    match tz.from_local_datetime(&naive) {
        Single(local) => Some(local.with_timezone(&Utc)),
        Ambiguous(early, _late) => Some(early.with_timezone(&Utc)),
        None => {
            // DST gap; nudge forward by one hour.
            let bumped = naive.checked_add_signed(chrono::Duration::hours(1))?;
            tz.from_local_datetime(&bumped)
                .single()
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
}
