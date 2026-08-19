//! Trip audit: match apex parent trips against ETIT GPS history, compare
//! each leg to the OSRM optimal route, and flag inefficiencies for review.
//!
//! Runs like the dead-zone scanner: a nightly pass over recent days plus an
//! on-demand admin trigger, results persisted to SQLite and served to the
//! dashboard via `/api/v1/trip-audit/*`.
//!
//! - [`pg`]        — read-only queries against the apex Postgres DB
//! - [`osrm`]      — OSRM `/route` client with time-gate exclusion
//! - [`geo`]       — polyline decode + point-to-track distance
//! - [`matcher`]   — geofence-based trip reconstruction and metrics
//! - [`calibrate`] — GPS stop clustering → pin suggestions
//! - [`scan`]      — orchestration + nightly scheduler

pub mod calibrate;
pub mod geo;
pub mod matcher;
pub mod osrm;
pub mod pg;
pub mod scan;

use serde::Serialize;

/// A geofenced place the matcher can detect visits to.
#[derive(Debug, Clone)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lng: f64,
    pub radius_m: f64,
}

/// A parent trip as loaded from apex Postgres, ready for matching.
#[derive(Debug, Clone)]
pub struct PlannedTrip {
    pub parent_trip_id: i64,
    pub day_local: String,
    pub company: String,
    pub terminal_name: String,
    pub car_no_plate: String,
    pub driver_name: String,
    /// ETIT GUID from cars.etit_car_id; empty when the car is not linked.
    pub etit_car_id: String,
    /// Resolved terminal geofence (via terminals + terminal_aliases).
    pub terminal: Option<Place>,
    /// One entry per container trip; place is None when the drop-off has no
    /// pin yet.
    pub deliveries: Vec<PlannedDelivery>,
}

#[derive(Debug, Clone)]
pub struct PlannedDelivery {
    pub trip_id: i64,
    pub drop_off_point: String,
    pub place: Option<Place>,
    /// Contracted one-way distance from fee_mappings, km.
    pub expected_km: Option<f64>,
}

/// Outcome of matching one planned trip against GPS history.
#[derive(Debug)]
pub struct MatchOutcome {
    pub status: MatchStatus,
    pub unmatched_reason: Option<&'static str>,
    pub start_ts: Option<i64>,
    pub end_ts: Option<i64>,
    pub returned_to_terminal: bool,
    pub deliveries_expected: i64,
    pub deliveries_visited: i64,
    pub legs: Vec<MatchedLeg>,
    pub flags: Vec<Flag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchStatus {
    Matched,
    Partial,
    Unmatched,
}

impl MatchStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MatchStatus::Matched => "matched",
            MatchStatus::Partial => "partial",
            MatchStatus::Unmatched => "unmatched",
        }
    }
}

#[derive(Debug)]
pub struct MatchedLeg {
    pub seq: i64,
    pub leg_type: &'static str, // outbound|between|return
    pub from_name: String,
    pub to_name: String,
    pub from_coords: (f64, f64),
    pub to_coords: (f64, f64),
    pub depart_ts: i64,
    pub arrive_ts: i64,
    pub night_window: bool,
    pub actual_km: f64,
    pub osrm_km: Option<f64>,
    pub osrm_secs: Option<i64>,
    pub max_deviation_m: Option<f64>,
    pub actual_geometry: String,
    pub osrm_geometry: String,
    /// Index into MatchOutcome.flags for flags belonging to this leg.
    pub flag_idx: Vec<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Flag {
    pub flag_type: &'static str,
    pub severity: &'static str,
    pub details: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct TripScanSummary {
    pub run_id: i64,
    pub status: String,
    pub days_scanned: i64,
    pub trips_scanned: i64,
    pub trips_matched: i64,
    pub trips_unmatched: i64,
    pub flags_raised: i64,
    pub suggestions_made: i64,
    pub error: Option<String>,
}
