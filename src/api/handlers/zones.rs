use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::zones::{DeadZone, ScanSummary};
use crate::zones::scan::{run_scan_window, ScanWindow, MAX_MANUAL_RANGE_DAYS};

#[derive(Deserialize)]
pub struct ListZonesQuery {
    pub active: Option<bool>,
}

pub async fn list(
    State(s): State<AppState>,
    Query(q): Query<ListZonesQuery>,
) -> AppResult<Json<Vec<DeadZone>>> {
    let zones = s.zones_repo.list(q.active.unwrap_or(false)).await?;
    Ok(Json(zones))
}

pub async fn get_one(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<DeadZone>> {
    let zone = s.zones_repo.get_by_id(id).await?
        .ok_or_else(|| AppError::NotFound(format!("zone {id} not found")))?;
    Ok(Json(zone))
}

#[derive(Deserialize)]
pub struct CreateZoneReq {
    pub name: String,
    pub lat: f64,
    pub lng: f64,
    pub radius_m: f64,
    pub active: Option<bool>,
}

pub async fn create(
    State(s): State<AppState>,
    Json(payload): Json<CreateZoneReq>,
) -> AppResult<Json<DeadZone>> {
    if payload.name.trim().is_empty() {
        return Err(AppError::BadRequest("name cannot be empty".into()));
    }
    if payload.radius_m <= 0.0 {
        return Err(AppError::BadRequest("radius_m must be > 0".into()));
    }
    if payload.lat < -90.0 || payload.lat > 90.0 {
        return Err(AppError::BadRequest("lat must be between -90 and 90".into()));
    }
    if payload.lng < -180.0 || payload.lng > 180.0 {
        return Err(AppError::BadRequest("lng must be between -180 and 180".into()));
    }

    let active = payload.active.unwrap_or(true);
    let id = s.zones_repo.create(&payload.name, payload.lat, payload.lng, payload.radius_m, active).await?;
    
    get_one(State(s), Path(id)).await
}

#[derive(Deserialize)]
pub struct UpdateZoneReq {
    pub name: Option<String>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub radius_m: Option<f64>,
    pub active: Option<bool>,
}

pub async fn update(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateZoneReq>,
) -> AppResult<Json<DeadZone>> {
    let existing = s.zones_repo.get_by_id(id).await?
        .ok_or_else(|| AppError::NotFound(format!("zone {id} not found")))?;

    let name = payload.name.unwrap_or(existing.name);
    let lat = payload.lat.unwrap_or(existing.lat);
    let lng = payload.lng.unwrap_or(existing.lng);
    let radius_m = payload.radius_m.unwrap_or(existing.radius_m);
    let active = payload.active.unwrap_or(existing.active);

    if name.trim().is_empty() {
        return Err(AppError::BadRequest("name cannot be empty".into()));
    }
    if radius_m <= 0.0 {
        return Err(AppError::BadRequest("radius_m must be > 0".into()));
    }
    if !(-90.0..=90.0).contains(&lat) {
        return Err(AppError::BadRequest("lat must be between -90 and 90".into()));
    }
    if !(-180.0..=180.0).contains(&lng) {
        return Err(AppError::BadRequest("lng must be between -180 and 180".into()));
    }

    s.zones_repo.update(id, &name, lat, lng, radius_m, active).await?;
    get_one(State(s), Path(id)).await
}

pub async fn delete(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<serde_json::Value>> {
    let affected = s.zones_repo.soft_delete(id).await?;
    if affected == 0 {
        return Err(AppError::NotFound(format!("zone {id} not found")));
    }
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Optional body for `POST /zones/scan`. Given `from` and `to` (local
/// `YYYY-MM-DD` days, inclusive) the scan runs in manual mode: it reports the
/// violations it found in the response and sends no WhatsApp alert. With no
/// body it runs the scheduled round over the configured lookback, alerts and
/// all — which is what the nightly timer calls.
#[derive(Deserialize, Default)]
pub struct ScanReq {
    pub from: Option<String>,
    pub to: Option<String>,
}

pub async fn run_scan_now(
    State(s): State<AppState>,
    body: Bytes,
) -> AppResult<Json<ScanSummary>> {
    // Read as bytes rather than `Option<Json<_>>`: that extractor turns any
    // rejection — a typo'd body, a missing content-type — into `None`, and
    // `None` here means "scheduled round", which alerts the WhatsApp group. A
    // malformed manual request must fail, not notify everyone.
    let req: ScanReq = if body.is_empty() {
        ScanReq::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| AppError::BadRequest(format!("invalid scan body: {e}")))?
    };
    let window = parse_window(req)?;
    let summary = run_scan_window(s, window).await;
    Ok(Json(summary))
}

fn parse_window(req: ScanReq) -> AppResult<ScanWindow> {
    let (from, to) = match (req.from.as_deref(), req.to.as_deref()) {
        (None, None) => return Ok(ScanWindow::Scheduled),
        // Half a range is a client bug, not a shorthand: guessing the other end
        // would silently scan — and report on — days nobody asked about.
        (Some(_), None) | (None, Some(_)) => {
            return Err(AppError::BadRequest("from and to must be given together".into()))
        }
        (Some(f), Some(t)) => (f, t),
    };

    let parse = |label: &str, raw: &str| {
        chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::BadRequest(format!("{label} must be a YYYY-MM-DD date")))
    };
    let from = parse("from", from)?;
    let to = parse("to", to)?;

    if to < from {
        return Err(AppError::BadRequest("to must not be before from".into()));
    }
    let span = (to - from).num_days() + 1;
    if span > MAX_MANUAL_RANGE_DAYS {
        return Err(AppError::BadRequest(format!(
            "range must span at most {MAX_MANUAL_RANGE_DAYS} days (got {span})"
        )));
    }

    Ok(ScanWindow::Manual { from, to })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zones::scan::ScanWindow;

    fn req(from: Option<&str>, to: Option<&str>) -> ScanReq {
        ScanReq {
            from: from.map(str::to_string),
            to: to.map(str::to_string),
        }
    }

    #[test]
    fn no_dates_runs_the_scheduled_round() {
        assert!(matches!(
            parse_window(req(None, None)).unwrap(),
            ScanWindow::Scheduled
        ));
    }

    #[test]
    fn a_full_range_runs_manually() {
        let w = parse_window(req(Some("2026-09-01"), Some("2026-09-03"))).unwrap();
        match w {
            ScanWindow::Manual { from, to } => {
                assert_eq!(from.to_string(), "2026-09-01");
                assert_eq!(to.to_string(), "2026-09-03");
            }
            _ => panic!("expected a manual window"),
        }
    }

    #[test]
    fn a_single_day_is_a_valid_range() {
        assert!(matches!(
            parse_window(req(Some("2026-09-01"), Some("2026-09-01"))).unwrap(),
            ScanWindow::Manual { .. }
        ));
    }

    // Half a range must not quietly become a scheduled round — that round
    // alerts the WhatsApp group.
    #[test]
    fn half_a_range_is_rejected() {
        assert!(parse_window(req(Some("2026-09-01"), None)).is_err());
        assert!(parse_window(req(None, Some("2026-09-01"))).is_err());
    }

    #[test]
    fn a_backwards_or_unparseable_range_is_rejected() {
        assert!(parse_window(req(Some("2026-09-03"), Some("2026-09-01"))).is_err());
        assert!(parse_window(req(Some("01/09/2026"), Some("03/09/2026"))).is_err());
    }

    #[test]
    fn a_range_longer_than_the_cap_is_rejected() {
        let from = "2026-01-01";
        let ok = chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d").unwrap()
            + chrono::Days::new(MAX_MANUAL_RANGE_DAYS as u64 - 1);
        let too_long = ok + chrono::Days::new(1);
        assert!(parse_window(req(Some(from), Some(&ok.to_string()))).is_ok());
        assert!(parse_window(req(Some(from), Some(&too_long.to_string()))).is_err());
    }
}
