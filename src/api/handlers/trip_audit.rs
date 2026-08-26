//! Trip-audit API: matches + legs + flags for the dashboard review page,
//! calibration suggestions for the locations inbox, and an on-demand scan
//! trigger. Mirrors the zones API surface.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::db::trip_audit::{SuggestionRow, TripFlagRow, TripLegRow, TripMatchRow, TripScanRunRow};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::trips::scan::run_scan;
use crate::trips::TripScanSummary;

#[derive(Deserialize)]
pub struct ListMatchesQuery {
    /// Inclusive local-day bounds (YYYY-MM-DD).
    pub from: Option<String>,
    pub to: Option<String>,
    pub status: Option<String>,
    pub company: Option<String>,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default)]
    pub unreviewed: bool,
    /// "severity" (review queue order) or "date" (default).
    pub sort: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[derive(Serialize)]
pub struct PageEnvelope<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

pub async fn list_matches(
    State(s): State<AppState>,
    Query(q): Query<ListMatchesQuery>,
) -> AppResult<Json<PageEnvelope<TripMatchRow>>> {
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(25).clamp(1, 200);
    let (items, total) = s
        .trip_repo
        .list_matches(
            q.from.as_deref(),
            q.to.as_deref(),
            q.status.as_deref(),
            q.flagged,
            q.unreviewed,
            q.company.as_deref(),
            q.sort.as_deref().unwrap_or("date"),
            page,
            per_page,
        )
        .await?;
    Ok(Json(PageEnvelope { items, total, page, per_page }))
}

#[derive(Deserialize)]
pub struct SummaryQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub company: Option<String>,
}

#[derive(Serialize)]
pub struct SummaryResponse {
    #[serde(flatten)]
    pub totals: crate::db::trip_audit::MatchSummaryRow,
    /// sum(osrm) / sum(actual) over compared trips, percent. None until at
    /// least one trip has an OSRM comparison.
    pub efficiency_pct: Option<f64>,
    pub excess_km: f64,
    pub worst_routes: Vec<crate::db::trip_audit::WorstRouteRow>,
}

/// Whole-window KPI aggregates — exact regardless of pagination.
pub async fn summary(
    State(s): State<AppState>,
    Query(q): Query<SummaryQuery>,
) -> AppResult<Json<SummaryResponse>> {
    let totals = s
        .trip_repo
        .match_summary(q.from.as_deref(), q.to.as_deref(), q.company.as_deref())
        .await?;
    let worst_routes = s
        .trip_repo
        .worst_routes(q.from.as_deref(), q.to.as_deref(), q.company.as_deref(), 3)
        .await?;
    let efficiency_pct = if totals.actual_km_compared > 0.0 {
        Some((totals.osrm_km / totals.actual_km_compared * 1000.0).round() / 10.0)
    } else {
        None
    };
    let excess_km =
        ((totals.actual_km_compared - totals.osrm_km).max(0.0) * 10.0).round() / 10.0;
    Ok(Json(SummaryResponse { totals, efficiency_pct, excess_km, worst_routes }))
}

#[derive(Serialize)]
pub struct MatchDetail {
    #[serde(flatten)]
    pub m: TripMatchRow,
    pub legs: Vec<TripLegRow>,
    pub flags: Vec<TripFlagRow>,
}

pub async fn get_match(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<MatchDetail>> {
    let m = s
        .trip_repo
        .get_match(id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("trip match {id} not found")))?;
    let legs = s.trip_repo.get_legs(id).await?;
    let flags = s.trip_repo.get_flags(id).await?;
    Ok(Json(MatchDetail { m, legs, flags }))
}

#[derive(Deserialize)]
pub struct ReviewReq {
    pub note: Option<String>,
}

pub async fn review_match(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ReviewReq>,
) -> AppResult<Json<serde_json::Value>> {
    let ok = s.trip_repo.set_reviewed(id, req.note.as_deref()).await?;
    if !ok {
        return Err(AppError::NotFound(format!("trip match {id} not found")));
    }
    Ok(Json(serde_json::json!({ "reviewed": true })))
}

#[derive(Deserialize)]
pub struct RunScanReq {
    /// Optional explicit days (YYYY-MM-DD); defaults to the configured
    /// lookback window.
    #[serde(default)]
    pub dates: Vec<String>,
}

pub async fn run_scan_now(
    State(s): State<AppState>,
    payload: Option<Json<RunScanReq>>,
) -> AppResult<Json<TripScanSummary>> {
    let dates = payload
        .map(|Json(r)| r.dates)
        .unwrap_or_default()
        .iter()
        .map(|d| {
            NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .map_err(|_| AppError::BadRequest(format!("invalid date `{d}`")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let summary = run_scan(s, if dates.is_empty() { None } else { Some(dates) }).await;
    Ok(Json(summary))
}

pub async fn list_runs(State(s): State<AppState>) -> AppResult<Json<Vec<TripScanRunRow>>> {
    Ok(Json(s.trip_repo.list_runs(30).await?))
}

#[derive(Deserialize)]
pub struct SuggestionsQuery {
    pub status: Option<String>,
}

pub async fn list_suggestions(
    State(s): State<AppState>,
    Query(q): Query<SuggestionsQuery>,
) -> AppResult<Json<Vec<SuggestionRow>>> {
    Ok(Json(s.trip_repo.list_suggestions(q.status.as_deref()).await?))
}

#[derive(Deserialize)]
pub struct AckSuggestionReq {
    /// "accepted" or "dismissed".
    pub status: String,
}

pub async fn ack_suggestion(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<AckSuggestionReq>,
) -> AppResult<Json<serde_json::Value>> {
    if req.status != "accepted" && req.status != "dismissed" {
        return Err(AppError::BadRequest("status must be accepted|dismissed".into()));
    }
    let ok = s.trip_repo.ack_suggestion(id, &req.status).await?;
    if !ok {
        return Err(AppError::NotFound(format!("suggestion {id} not found")));
    }
    Ok(Json(serde_json::json!({ "status": req.status })))
}
