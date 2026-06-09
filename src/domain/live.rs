//! `LiveVehicleStatus` — one current snapshot per vehicle.

use serde::{Deserialize, Serialize};

use super::time::Timestamp;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveVehicleStatus {
    pub id: String,
    pub plate: Option<String>,
    pub speed: i32,
    pub status: i32,
    pub status_label: String,
    pub lat: f64,
    pub lng: f64,
    pub timestamp: Option<Timestamp>,
    pub event: Option<String>,
    pub sensor_data: Option<String>,
}
