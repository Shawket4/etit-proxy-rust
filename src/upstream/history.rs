//! Integration C — route history via `/WebPages/GetAllHistoryData.aspx`.
//!
//! The upstream expects `M/D/YYYY HH:MM:SS` *in its own timezone*.

use serde::Deserialize;

use crate::domain::history::{SensorEvent, Stop};
use crate::domain::time::{etit_format_for_history, parse_etit_local, parse_etit_local_after, Timestamp};
use crate::domain::HistoryPoint;
use crate::error::AppError;

use super::client::EtitClient;
use super::parsers::{fix_unquoted_json_keys, pct, truncate_for_log};

const PATH: &str = "/WebPages/GetAllHistoryData.aspx";

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Envelope {
    Wrapped {
        #[serde(default)]
        history: Vec<Row>,
        #[serde(default)]
        stops: Vec<StopRow>,
        #[serde(default, rename = "Sensors")]
        sensors: Vec<SensorRow>,
    },
    Array(Vec<Row>),
}

impl Envelope {
    fn into_parts(self) -> (Vec<Row>, Vec<StopRow>, Vec<SensorRow>) {
        match self {
            Envelope::Wrapped {
                history,
                stops,
                sensors,
            } => (history, stops, sensors),
            Envelope::Array(rows) => (rows, Vec::new(), Vec::new()),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Row {
    #[serde(default)] p: Vec<Pt>,
    #[serde(default)] d: String,
    #[serde(default)] s: String,
    #[serde(default)] l: String,
    #[serde(flatten)] _extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct Pt {
    #[serde(default)] a: String,
    #[serde(default)] o: String,
}

#[derive(Debug, Clone, Deserialize)]
struct StopRow {
    lat: String,
    lon: String,
    from: String,
    to: String,
    duration: String,
    #[serde(default)]
    address: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SensorRow {
    lat: String,
    lon: String,
    #[serde(rename = "strtDate")]
    start_date: String,
    #[serde(rename = "typeName")]
    type_name: String,
}

pub struct UpstreamHistory {
    pub points: Vec<HistoryPoint>,
    pub stops: Vec<Stop>,
    pub sensors: Vec<SensorEvent>,
}

/// Fetch a range of history. `from` and `to` are UTC; we convert to the
/// proxy's configured timezone for the upstream call.
pub async fn fetch_range(
    client: &EtitClient,
    vehicle_id: &str,
    from: Timestamp,
    to: Timestamp,
) -> Result<UpstreamHistory, AppError> {
    if vehicle_id.is_empty() {
        return Err(AppError::BadRequest("vehicle id required".into()));
    }
    let tz = client.config().timezone;

    let from_str = etit_format_for_history(from, tz);
    let to_str = etit_format_for_history(to, tz);

    let path = format!(
        "{PATH}?id={id}&time=6&points=1&from={from_q}&to={to_q}",
        id = pct(vehicle_id),
        from_q = pct(&from_str),
        to_q = pct(&to_str),
    );

    let resp = client.get(&path).await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::UpstreamStatus {
            status: status.as_u16(),
            body: truncate_for_log(&body, 400),
        });
    }
    let raw = resp
        .text()
        .await
        .map_err(|e| AppError::Parse(format!("history read: {e}")))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(UpstreamHistory {
            points: Vec::new(),
            stops: Vec::new(),
            sensors: Vec::new(),
        });
    }

    // Try strict parse first; fall back to the "fixed" version only when
    // the strict parser fails — avoids unnecessary regex work.
    let v: serde_json::Value = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => v,
        Err(_) => {
            let fixed = fix_unquoted_json_keys(trimmed);
            serde_json::from_str(&fixed).map_err(|e| {
                // The shape of the body, never the body. This is an error, so
                // sentry-tracing turns it into an event and every field on it
                // is exported; the payload is a day of GPS history, which is
                // the coordinates the scrubber exists to keep out. Length plus
                // "did ETIT hand us an HTML error page instead" is what
                // actually diagnoses this, and serde already says where it
                // broke. Compliance control -- do not put the body back.
                tracing::error!(
                    vehicle_id,
                    body_len = fixed.len(),
                    looks_like_html = fixed.trim_start().starts_with('<'),
                    error = %e,
                    body = %crate::observability::sanitize_message(&truncate_for_log(&fixed, 400)),
                    "history JSON parse failed even after fix"
                );
                AppError::Parse(format!("history JSON: {e}"))
            })?
        }
    };

    let envelope: Envelope = serde_json::from_value(v)
        .map_err(|e| AppError::Parse(format!("history structural: {e}")))?;

    Ok(flatten(envelope, tz))
}

fn flatten(env: Envelope, tz: chrono_tz::Tz) -> UpstreamHistory {
    let (history_rows, stop_rows, sensor_rows) = env.into_parts();

    // Every timestamp in this payload is Cairo wall clock (`TIMEZONE`) — the
    // same zone the query was sent in; see `domain::time` for the per-field
    // table. Rows arrive in chronological order, which is what lets the
    // autumn repeated hour resolve forward instead of folding back.
    let mut points = Vec::new();
    let mut prev: Option<Timestamp> = None;
    for row in history_rows {
        let speed = row
            .s
            .parse::<f64>()
            .ok()
            .map(|f| f.max(0.0).round() as u32)
            .unwrap_or(0);
        let speed_limit = row
            .l
            .parse::<f64>()
            .ok()
            .map(|f| f.max(0.0).round() as u32)
            .unwrap_or(0);
        let timestamp = parse_etit_local_after(&row.d, tz, prev);
        if timestamp.is_some() {
            prev = timestamp;
        }

        for pt in row.p {
            let lat = pt.a.parse::<f64>().unwrap_or(0.0);
            let lng = pt.o.parse::<f64>().unwrap_or(0.0);
            points.push(HistoryPoint {
                lat,
                lng,
                timestamp,
                speed,
                speed_limit,
            });
        }
    }

    use crate::domain::history::{SensorEvent, Stop};

    let mut stops = Vec::new();
    for s in stop_rows {
        let lat = s.lat.parse::<f64>().unwrap_or(0.0);
        let lng = s.lon.parse::<f64>().unwrap_or(0.0);
        let from = parse_etit_local(&s.from, tz);
        let to = parse_etit_local_after(&s.to, tz, from).unwrap_or_default();
        let from = from.unwrap_or_default();
        stops.push(Stop {
            lat,
            lng,
            from,
            to,
            duration: s.duration,
            address: s.address,
        });
    }

    let mut sensors = Vec::new();
    for s in sensor_rows {
        let lat = s.lat.parse::<f64>().unwrap_or(0.0);
        let lng = s.lon.parse::<f64>().unwrap_or(0.0);
        let timestamp = parse_etit_local(sensor_date(&s.start_date), tz).unwrap_or_default();
        sensors.push(SensorEvent {
            lat,
            lng,
            timestamp,
            type_name: s.type_name,
        });
    }

    UpstreamHistory {
        points,
        stops,
        sensors,
    }
}

/// The date inside a sensor's `strtDate`, which arrives labelled —
/// `"Start Time : 25/04/2026 01:06:34"`. Everything before the first digit is
/// the label. (Splitting on the first `:` instead would cut a bare
/// `25/04/2026 01:06:34` at the hour and drop the event to 1970.)
fn sensor_date(raw: &str) -> &str {
    let s = raw.trim();
    match s.find(|c: char| c.is_ascii_digit()) {
        Some(i) => s[i..].trim(),
        None => s,
    }
}

#[derive(Debug, Clone, Deserialize)]
struct TripSummaryEnvelope {
    #[serde(rename = "TripSummary")]
    summary: Vec<TripSummaryRow>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TripSummaryRow {
    total_mileage: String,
    total_active_time: String,
    total_passive_time: String,
    total_idle_time: String,
    driver_name: String,
    #[serde(rename = "NumberofStops")]
    number_of_stops: String,
    #[serde(rename = "TotalDisConnectedTime")]
    total_disconnected_time: String,
    total_fuel_consumption: String,
    sensor1: String, // e.g. "26#Ignition Off"
    sensor2: String, // e.g. "25#Ignition On"
}

pub async fn fetch_trip_summary(
    client: &EtitClient,
    vehicle_id: &str,
    from: Timestamp,
    to: Timestamp,
) -> Result<crate::domain::history::TripSummary, AppError> {
    let tz = client.config().timezone;
    let from_str = etit_format_for_history(from, tz);
    let to_str = etit_format_for_history(to, tz);

    let path = format!(
        "/WebPages/GetHistoryTripSummary.ashx?id={id}&time=6&from={from_q}&to={to_q}&t={t}",
        id = pct(vehicle_id),
        from_q = pct(&from_str),
        to_q = pct(&to_str),
        t = chrono::Utc::now().timestamp_millis(),
    );

    let resp = client.get(&path).await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::UpstreamStatus {
            status: status.as_u16(),
            body: truncate_for_log(&body, 400),
        });
    }

    let raw = resp
        .text()
        .await
        .map_err(|e| AppError::Parse(format!("summary read: {e}")))?;

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(crate::domain::history::TripSummary::default());
    }

    // ETIT returns "TripSummary:[...]" without braces and with unquoted keys.
    let wrapped = if !trimmed.starts_with('{') {
        format!("{{ {trimmed} }}")
    } else {
        trimmed.to_string()
    };
    let fixed = fix_unquoted_json_keys(&wrapped);

    let envelope: TripSummaryEnvelope = serde_json::from_str(&fixed).map_err(|e| {
        // Shape only -- see the note on the history parse above.
        tracing::error!(
            vehicle_id,
            body_len = fixed.len(),
            looks_like_html = fixed.trim_start().starts_with('<'),
            error = %e,
            body = %crate::observability::sanitize_message(&truncate_for_log(&fixed, 400)),
            "trip summary JSON parse failed"
        );
        AppError::Parse(format!("summary JSON: {e}"))
    })?;

    let row = envelope.summary.into_iter().next().ok_or_else(|| {
        AppError::Parse("empty trip summary array".into())
    })?;

    use crate::domain::history::TripSummary;

    Ok(TripSummary {
        total_mileage: row.total_mileage,
        total_active_time: row.total_active_time,
        total_passive_time: row.total_passive_time,
        total_idle_time: row.total_idle_time,
        driver_name: row.driver_name,
        number_of_stops: row.number_of_stops,
        total_disconnected_time: row.total_disconnected_time,
        total_fuel_consumption: row.total_fuel_consumption,
        ignition_off_count: row.sensor1.split('#').next().unwrap_or("0").to_string(),
        ignition_on_count: row.sensor2.split('#').next().unwrap_or("0").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn utc(s: &str) -> Timestamp {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const CAIRO: chrono_tz::Tz = chrono_tz::Africa::Cairo;

    fn parse(body: &str) -> UpstreamHistory {
        let fixed = fix_unquoted_json_keys(body);
        let v: serde_json::Value = serde_json::from_str(body)
            .or_else(|_| serde_json::from_str(&fixed))
            .expect("json");
        flatten(serde_json::from_value(v).expect("envelope"), CAIRO)
    }

    #[test]
    fn history_stops_and_sensors_are_cairo_wall_clock() {
        let h = parse(
            r#"{"history":[{"p":[{"a":"26.84","o":"33.92"}],"d":"25/10/2026 10:18:00","s":"52","l":"80"}],
                "stops":[{"lat":"26.8","lon":"33.9","from":"25/10/2026 06:00:00","to":"25/10/2026 07:30:00","duration":"1h 30m","address":"x"}],
                "Sensors":[{"lat":"26.8","lon":"33.9","strtDate":"Start Time : 25/10/2026 01:06:34","typeName":"Ignition On"}]}"#,
        );
        assert_eq!(h.points[0].timestamp, Some(utc("2026-10-25T07:18:00Z")));
        assert_eq!(h.stops[0].from, utc("2026-10-25T03:00:00Z"));
        assert_eq!(h.stops[0].to, utc("2026-10-25T04:30:00Z"));
        assert_eq!(h.sensors[0].timestamp, utc("2026-10-24T22:06:34Z"));
    }

    #[test]
    fn sensor_date_with_or_without_label() {
        assert_eq!(sensor_date("Start Time : 25/04/2026 01:06:34"), "25/04/2026 01:06:34");
        assert_eq!(sensor_date("25/04/2026 01:06:34"), "25/04/2026 01:06:34");
        assert_eq!(sensor_date("  "), "");
    }

    /// The repeated hour on 2026-10-29: a track crossing 24:00 EEST → 23:00
    /// EET must stay monotonic rather than jump back an hour.
    #[test]
    fn history_track_through_fall_back_stays_monotonic() {
        let h = parse(
            r#"[{"p":[{"a":"1","o":"1"}],"d":"29/10/2026 23:50:00","s":"0","l":"0"},
                {"p":[{"a":"1","o":"1"}],"d":"29/10/2026 23:10:00","s":"0","l":"0"},
                {"p":[{"a":"1","o":"1"}],"d":"29/10/2026 23:55:00","s":"0","l":"0"},
                {"p":[{"a":"1","o":"1"}],"d":"30/10/2026 00:05:00","s":"0","l":"0"}]"#,
        );
        let ts: Vec<_> = h.points.iter().map(|p| p.timestamp.unwrap()).collect();
        assert_eq!(
            ts,
            vec![
                utc("2026-10-29T20:50:00Z"), // 23:50 EEST
                utc("2026-10-29T21:10:00Z"), // 23:10 EET (second pass)
                utc("2026-10-29T21:55:00Z"), // 23:55 EET
                utc("2026-10-29T22:05:00Z"), // 00:05 EET
            ],
        );
    }
}
