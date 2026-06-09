//! `/api/v1/vehicles/:id/history` handler.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::NaiveDate;

use crate::api::dto::HistoryQuery;
use crate::domain::history::HistoryResponse;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// `GET /api/v1/vehicles/:id/history?date=YYYY-MM-DD`  
/// `GET /api/v1/vehicles/:id/history?from=...&to=...`
///
/// Without any query parameters, defaults to **today** in the configured
/// timezone. `date` and `from`/`to` are mutually exclusive: providing both
/// is a `BAD_REQUEST`.
pub async fn for_vehicle(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> AppResult<Json<HistoryResponse>> {
    if id.is_empty() {
        return Err(AppError::BadRequest("vehicle id required".into()));
    }

    let force = q.refresh.unwrap_or(false);

    match (q.date.as_deref(), q.from.as_deref(), q.to.as_deref()) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(AppError::BadRequest(
            "use either `date` OR `from`+`to`, not both".into(),
        )),
        (_, Some(f), Some(t)) => Ok(Json(s.history.for_range(&id, f, t, force).await?)),
        (_, Some(_), None) | (_, None, Some(_)) => Err(AppError::BadRequest(
            "`from` and `to` must be provided together".into(),
        )),
        (Some(d), _, _) => {
            let date = NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d").map_err(|e| {
                AppError::BadRequest(format!("invalid `date` (YYYY-MM-DD): {e}"))
            })?;
            Ok(Json(s.history.for_day(&id, date, force).await?))
        }
        (None, None, None) => {
            let today = s.cache.today_local();
            Ok(Json(s.history.for_day(&id, today, force).await?))
        }
    }
}

/// `GET /api/v1/vehicles/:id/history/summary?from=...&to=...`
pub async fn summary(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> AppResult<Json<crate::domain::history::TripSummary>> {
    let (from, to) = match (q.date.as_deref(), q.from.as_deref(), q.to.as_deref()) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            return Err(AppError::BadRequest(
                "use either `date` OR `from`+`to`, not both".into(),
            ))
        }
        (_, Some(f), Some(t)) => (f.to_string(), t.to_string()),
        (_, Some(_), None) | (_, None, Some(_)) => {
            return Err(AppError::BadRequest(
                "`from` and `to` must be provided together".into(),
            ))
        }
        (Some(d), _, _) => {
            // date shortcut -> full day range
            let from = format!("{d} 00:00:00");
            let to = format!("{d} 23:59:59");
            (from, to)
        }
        (None, None, None) => {
            let today = s.cache.today_local();
            let from = today.format("%Y-%m-%d 00:00:00").to_string();
            let to = today.format("%Y-%m-%d 23:59:59").to_string();
            (from, to)
        }
    };

    Ok(Json(s.history.get_trip_summary(&id, &from, &to).await?))
}
