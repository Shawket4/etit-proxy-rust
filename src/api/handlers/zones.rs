use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::zones::{DeadZone, ScanSummary};
use crate::zones::scan::run_scan;

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

pub async fn run_scan_now(
    State(s): State<AppState>,
) -> AppResult<Json<ScanSummary>> {
    let summary = run_scan(s).await;
    Ok(Json(summary))
}
