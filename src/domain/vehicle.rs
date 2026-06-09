//! `Vehicle` — frontend-facing metadata about a tracked unit.

use serde::{Deserialize, Serialize};

use super::time::Timestamp;

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

    #[serde(skip_serializing_if = "serde_json::Map::is_empty", default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Vehicle {
    /// Best-effort plate extraction from the upstream's "CODE/PLATE" codename.
    pub fn split_codename(codename: &str) -> (&str, &str) {
        match codename.split_once('/') {
            Some((code, plate)) => (code.trim(), plate.trim()),
            None => (codename.trim(), ""),
        }
    }
}
