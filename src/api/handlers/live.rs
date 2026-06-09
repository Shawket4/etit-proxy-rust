//! `/api/v1/vehicles/live` and `/api/v1/vehicles/:id/live` handlers.

use axum::extract::{Path, State};
use axum::Json;

use crate::domain::LiveVehicleStatus;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// `GET /api/v1/vehicles/live`
pub async fn list(State(s): State<AppState>) -> AppResult<Json<Vec<LiveVehicleStatus>>> {
    Ok(Json(s.live.list().await?))
}

/// `GET /api/v1/vehicles/:id/live`
pub async fn get_one(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<LiveVehicleStatus>> {
    if id.is_empty() {
        return Err(AppError::BadRequest("vehicle id required".into()));
    }
    Ok(Json(s.live.get_by_id(&id).await?))
}
