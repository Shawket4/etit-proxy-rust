//! Pure data types passed between layers. No I/O, no derived globals.
//!
//! Intentionally minimal: each layer can map *into* these from raw upstream
//! payloads and *from* these into JSON responses without coupling on the
//! transport details.

pub mod history;
pub mod live;
pub mod time;
pub mod vehicle;

pub use history::{HistoryPoint, HistoryResponse};
pub use live::LiveVehicleStatus;
pub use time::{utc_now, Timestamp};
pub use vehicle::Vehicle;

/// Upstream status code → human label. The mapping is part of the public
/// API contract (see `API.md` §3); changing it is a breaking change.
pub fn status_label(code: i32) -> &'static str {
    match code {
        1 | 4    => "Moving",
        3        => "Stopped (Ignition Off)",
        12       => "Offline",
        13       => "Idling (Ignition On)",
        17 | 18  => "Geofence Violation",
        _        => "Unknown",
    }
}
