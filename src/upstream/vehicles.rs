//! Integration A — vehicle metadata via
//! `/WebPages/maps.aspx/GetTransportersPage`.

use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::{status_label, time::parse_etit_local, Vehicle};
use crate::error::AppError;

use super::client::EtitClient;
use super::parsers::truncate_for_log;

const PATH: &str = "/WebPages/maps.aspx/GetTransportersPage";

/// Cap to defend against a buggy upstream returning a non-empty `d` forever.
const PAGE_HARD_CAP: u32 = 200;

#[derive(Debug, Clone, Serialize)]
struct PageRequest {
    #[serde(rename = "pageNumber")]   page_number: u32,
    #[serde(rename = "filterCategory")] filter_category: &'static str,
    #[serde(rename = "filterValue")]    filter_value: &'static str,
    #[serde(rename = "filterText")]     filter_text: &'static str,
    #[serde(rename = "subID")]          sub_id: String,
    #[serde(rename = "staffID")]        staff_id: String,
}

/// Raw upstream row. Every field arrives as a string.
#[derive(Debug, Clone, Deserialize)]
struct UpstreamRow {
    #[serde(rename = "ID", default)]               id: String,
    #[serde(rename = "codename", default)]         codename: String,
    #[serde(rename = "SpeedLimit", default)]       speed_limit: Option<String>,
    #[serde(rename = "OnlineFlag", default)]       online_flag: Option<String>,
    #[serde(rename = "Speed", default)]            speed: Option<String>,
    #[serde(rename = "Status", default)]           status: Option<String>,
    #[serde(rename = "LastLocationDate", default)] last_location_date: Option<String>,
    #[serde(flatten)]                              extra: serde_json::Map<String, Value>,
}

pub async fn fetch_all(client: &EtitClient) -> Result<Vec<Vehicle>, AppError> {
    let auth = client.auth_snapshot().await;
    let auth = if !auth.is_ready() {
        client.force_login().await?;
        client.auth_snapshot().await
    } else {
        auth
    };

    let tz = client.config().timezone;
    let mut all = Vec::new();

    for page in 1..=PAGE_HARD_CAP {
        let req = PageRequest {
            page_number:     page,
            filter_category: "Default",
            filter_value:    "",
            filter_text:     "",
            sub_id:          auth.sub_id.clone(),
            staff_id:        auth.staff_id.clone(),
        };

        let resp = client.post_json(PATH, req).await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::UpstreamStatus {
                status: status.as_u16(),
                body:   truncate_for_log(&body, 400),
            });
        }
        let body = resp
            .text()
            .await
            .map_err(|e| AppError::Parse(format!("vehicles page {page} read: {e}")))?;

        let envelope: Value = serde_json::from_str(&body)
            .map_err(|e| AppError::Parse(format!("vehicles page {page} envelope: {e}")))?;

        let d_val = envelope
            .get("d")
            .ok_or_else(|| {
                AppError::Parse(format!("vehicles page {page} missing `d`"))
            })?;

        if d_val.is_null() {
            break;
        }
        let d_str = d_val
            .as_str()
            .ok_or_else(|| AppError::Parse(format!("vehicles page {page} `d` not string")))?;
        let trimmed = d_str.trim();
        if trimmed.is_empty() || trimmed == "[]" || trimmed == "null" {
            break;
        }

        let rows: Vec<UpstreamRow> = serde_json::from_str(trimmed)
            .map_err(|e| AppError::Parse(format!("vehicles page {page} inner: {e}")))?;

        if rows.is_empty() {
            break;
        }
        all.extend(rows.into_iter().map(|r| convert(r, tz)));
    }

    tracing::info!(count = all.len(), "fetched vehicles");
    Ok(all)
}

fn convert(row: UpstreamRow, tz: Tz) -> Vehicle {
    let speed_limit = row.speed_limit.as_deref().and_then(parse_u32).unwrap_or(0);
    let online      = row.online_flag.as_deref().map(parse_bool).unwrap_or(false);
    let speed       = row.speed.as_deref().and_then(parse_u32).unwrap_or(0);
    let status      = row.status.as_deref().and_then(parse_i32).unwrap_or(0);
    let status_label_str = status_label(status).to_string();

    let last_location_at = row
        .last_location_date
        .as_deref()
        .and_then(|s| parse_etit_local(s, tz));

    let (_, plate) = Vehicle::split_codename(&row.codename);
    let plate_str = plate.to_string();

    let extra = row.extra.into_iter().map(|(k, v)| (k, coerce(v))).collect();

    Vehicle {
        id:               row.id,
        codename:         row.codename,
        plate:            plate_str,
        speed_limit,
        online,
        speed,
        status,
        status_label:     status_label_str,
        last_location_at,
        extra,
    }
}

fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() { return None; }
    s.parse::<u32>()
        .ok()
        .or_else(|| s.parse::<f64>().ok().map(|f| f.max(0.0).round() as u32))
}

fn parse_i32(s: &str) -> Option<i32> {
    let s = s.trim();
    if s.is_empty() { return None; }
    s.parse::<i32>()
        .ok()
        .or_else(|| s.parse::<f64>().ok().map(|f| f.round() as i32))
}

fn parse_bool(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

fn coerce(v: Value) -> Value {
    match v {
        Value::String(s) => {
            let t = s.trim();
            if t.eq_ignore_ascii_case("true")  { return Value::Bool(true);  }
            if t.eq_ignore_ascii_case("false") { return Value::Bool(false); }
            if let Ok(n) = t.parse::<i64>()    { return Value::from(n);     }
            if let Ok(n) = t.parse::<f64>() {
                if let Some(num) = serde_json::Number::from_f64(n) {
                    return Value::Number(num);
                }
            }
            Value::String(s)
        }
        other => other,
    }
}
