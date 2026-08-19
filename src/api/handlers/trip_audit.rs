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
}

pub async fn list_matches(
    State(s): State<AppState>,
    Query(q): Query<ListMatchesQuery>,
) -> AppResult<Json<Vec<TripMatchRow>>> {
    let rows = s
        .trip_repo
        .list_matches(
            q.from.as_deref(),
            q.to.as_deref(),
            q.status.as_deref(),
            q.flagged,
            q.company.as_deref(),
        )
        .await?;
    Ok(Json(rows))
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
