//! `/api/v1/vehicles*` handlers.

use axum::extract::{Path, State};
use axum::Json;

use crate::domain::Vehicle;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// `GET /api/v1/vehicles`
pub async fn list(State(s): State<AppState>) -> AppResult<Json<Vec<Vehicle>>> {
    let list = s.vehicles.list().await?;
    Ok(Json((*list).clone()))
}

/// `GET /api/v1/vehicles/:id`
pub async fn get_one(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Vehicle>> {
    if id.is_empty() {
        return Err(AppError::BadRequest("vehicle id required".into()));
    }
    match s.vehicles.get_by_id(&id).await? {
        Some(v) => Ok(Json(v)),
        None    => Err(AppError::NotFound(format!("vehicle `{id}` not found"))),
    }
}
