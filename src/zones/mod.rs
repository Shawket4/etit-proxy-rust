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
    pub days_scanned: i64,
    pub vehicles_scanned: i64,
    pub violations_found: i64,
    pub alerts_sent: i64,
    pub error: Option<String>,
}
