//! Integration B — live vehicle status via
//! `/WebPages/GetTransportersSpeedStatus.aspx`.
//!
//! The upstream returns an asterisk-delimited columnar block:
//!
//! ```text
//! ids * speeds * statuses * lngs * lats * times * <extras…>
//! ```
//!
//! Required columns are 0..=5. Beyond that the layout has shifted across
//! deployments — so instead of hard-coding column indices for events and
//! sensor data, we **scan** every later column row-by-row and pick the
//! first row-cell that:
//!
//! - matches a `\d+#.+` shape → treat as an event (e.g. `4#Ignition On`).
//! - else, the first non-empty cell becomes `sensor_data`.
//!
//! This insulates us from upstream column reshuffles.

use chrono_tz::Tz;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::domain::{status_label, time::parse_etit_local, LiveVehicleStatus, Vehicle};
use crate::error::AppError;

use super::client::EtitClient;
use super::parsers::truncate_for_log;

const PATH: &str = "/WebPages/GetTransportersSpeedStatus.aspx";

static EVENT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\d+#.+").unwrap());

pub async fn fetch_live(
    client: &EtitClient,
    vehicles: &[Vehicle],
) -> Result<Vec<LiveVehicleStatus>, AppError> {
    if vehicles.is_empty() {
        return Ok(Vec::new());
    }
    let payload = build_ids_speedlimits(vehicles);
    let form = vec![("ids_speedlimits".to_string(), payload)];

    let resp = client.post_form(PATH, form).await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::UpstreamStatus {
            status: status.as_u16(),
            body:   truncate_for_log(&body, 400),
        });
    }
    let raw = resp.text().await?;
    parse_live(&raw, client.config().timezone, vehicles)
}

/// Build `id|limit|1,id|limit|1,…`. The trailing `|1` flag is required to
/// force coordinate inclusion in the response.
pub(crate) fn build_ids_speedlimits(vehicles: &[Vehicle]) -> String {
    let mut s = String::with_capacity(vehicles.len() * 64);
    for (i, v) in vehicles.iter().enumerate() {
        if i > 0 { s.push(','); }
        s.push_str(&v.id);
        s.push('|');
        s.push_str(&v.speed_limit.to_string());
        s.push('|');
        s.push('1');
    }
    s
}

pub(crate) fn parse_live(
    raw: &str,
    tz: Tz,
    vehicles: &[Vehicle],
) -> Result<Vec<LiveVehicleStatus>, AppError> {
    let raw = raw.trim();
    if raw.is_empty() { return Ok(Vec::new()); }

    let cols: Vec<Vec<&str>> = raw
        .split('*')
        .map(|c| c.split(',').map(|s| s.trim()).collect())
        .collect();

    if cols.len() < 6 {
        return Err(AppError::Parse(format!(
            "live response has {} columns, need ≥ 6",
            cols.len()
        )));
    }

    let row_count = cols[0].len();
    for (name, idx) in [("speeds", 1), ("statuses", 2), ("lngs", 3), ("lats", 4), ("times", 5)] {
        if cols[idx].len() != row_count {
            return Err(AppError::Parse(format!(
                "live column `{name}` has {} rows but ids has {}",
                cols[idx].len(), row_count
            )));
        }
    }

    // Plate lookup so the live response carries plates without a join.
    let plate_by_id: std::collections::HashMap<&str, &str> = vehicles
        .iter()
        .map(|v| (v.id.as_str(), v.plate.as_str()))
        .collect();

    let mut out = Vec::with_capacity(row_count);

    for i in 0..row_count {
        let id = cols[0][i].to_string();
        if id.is_empty() { continue; }

        let speed         = cols[1][i].parse::<i32>().unwrap_or(0);
        let status        = cols[2][i].parse::<i32>().unwrap_or(0);
        let status_label_ = status_label(status).to_string();
        let lng           = cols[3][i].parse::<f64>().unwrap_or(0.0);
        let lat           = cols[4][i].parse::<f64>().unwrap_or(0.0);
        // Cairo wall clock (`TIMEZONE`), unlike Tier A's UTC LastLocationDate.
        let timestamp     = parse_etit_local(cols[5][i], tz);

        // Scan extra columns for this row.
        let mut event:       Option<String> = None;
        let mut sensor_data: Option<String> = None;
        for col in cols.iter().skip(6) {
            let cell = col.get(i).copied().unwrap_or("").trim();
            if cell.is_empty() { continue; }
            if event.is_none() && EVENT_RE.is_match(cell) {
                // Strip the leading "<digits>#" prefix for display.
                if let Some(idx) = cell.find('#') {
                    event = Some(cell[idx + 1..].trim().to_string());
                } else {
                    event = Some(cell.to_string());
                }
            } else if sensor_data.is_none() {
                sensor_data = Some(cell.to_string());
            }
        }

        let plate = plate_by_id.get(id.as_str()).map(|p| p.to_string()).filter(|p| !p.is_empty());

        out.push(LiveVehicleStatus {
            id,
            plate,
            speed,
            status,
            status_label: status_label_,
            lat,
            lng,
            timestamp,
            event,
            sensor_data,
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Vehicle;

    fn v(id: &str, plate: &str, limit: u32) -> Vehicle {
        Vehicle {
            id: id.into(),
            codename: format!("OL/{plate}"),
            plate: plate.into(),
            speed_limit: limit,
            online: true,
            speed: 0,
            status: 1,
            status_label: "Moving".into(),
            last_location_at: None,
            lat: None,
            lng: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn ids_speedlimits_format() {
        let vs = vec![v("a", "ABC", 70), v("b", "XYZ", 80)];
        assert_eq!(build_ids_speedlimits(&vs), "a|70|1,b|80|1");
    }

    #[test]
    fn parses_two_row_payload_with_event_at_col_9() {
        let raw = "0ed629a4-aaaa,19cdd2ad-bbbb*\
                   8,52*\
                   1,1*\
                   33.923,33.924*\
                   26.843,26.844*\
                   26 April 2026 16:28,26 April 2026 16:28*\
                   -1,-1*\
                   0.1,0.1*\
                   -1,-1*\
                   4#Ignition On,4#Ignition On";
        let vs = vec![v("0ed629a4-aaaa", "P1", 80), v("19cdd2ad-bbbb", "P2", 80)];
        let out = parse_live(raw, chrono_tz::Africa::Cairo, &vs).expect("parse");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "0ed629a4-aaaa");
        assert_eq!(out[0].speed, 8);
        assert_eq!(out[0].status, 1);
        assert_eq!(out[0].status_label, "Moving");
        assert!((out[0].lat - 26.843).abs() < 1e-6);
        assert!((out[0].lng - 33.923).abs() < 1e-6);
        assert_eq!(out[0].plate.as_deref(), Some("P1"));
        assert_eq!(out[0].event.as_deref(), Some("Ignition On"));
        // "26 April 2026 16:28" is Cairo wall clock (EEST) → 13:28Z.
        assert_eq!(
            out[0].timestamp.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-04-26T13:28:00+00:00"),
        );
    }

    #[test]
    fn empty_input_yields_empty() {
        assert!(parse_live("", chrono_tz::UTC, &[]).unwrap().is_empty());
    }
}
