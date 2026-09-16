pub mod geo;
pub mod notify;
pub mod scan;
pub mod visits;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DeadZone {
    pub id: i64,
    pub name: String,
    pub lat: f64,
    pub lng: f64,
    pub radius_m: f64,
    #[serde(default = "default_active")]
    pub active: bool,
}

fn default_active() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct PointTS {
    pub ts: i64,
    pub lat: f64,
    pub lng: f64,
}

#[derive(Debug, Clone)]
pub struct Visit {
    pub entry: i64,
    pub exit: i64,
    pub points: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanSummary {
    pub run_id: i64,
    pub status: String,
    /// `"scheduled"` for the nightly/startup round, `"manual"` for an operator
    /// scan over an explicit date range. A manual round reports and never
    /// alerts, so clients key the "no WhatsApp was sent" wording off this.
    pub mode: String,
    /// First and last local day covered, inclusive — a manual scan echoes back
    /// the range it actually read.
    pub from: String,
    pub to: String,
    pub days_scanned: i64,
    pub vehicles_scanned: i64,
    pub violations_found: i64,
    pub alerts_sent: i64,
    /// The report rows. Only a manual scan fills this; a scheduled round leaves
    /// it empty because its output is the WhatsApp alerts, not a payload.
    pub violations: Vec<ScanViolation>,
    pub error: Option<String>,
}

/// One in-zone stretch as a manual scan reports it: already resolved to a
/// vehicle codename and a zone name, timestamps already in the configured
/// local zone, so the caller renders the row without a second lookup.
#[derive(Debug, Clone, Serialize)]
pub struct ScanViolation {
    pub vehicle_id: String,
    pub codename: String,
    pub zone_id: i64,
    pub zone_name: String,
    pub zone_lat: f64,
    pub zone_lng: f64,
    pub entry_local: String,
    pub exit_local: String,
    pub dwell_secs: i64,
    pub point_count: u32,
}
