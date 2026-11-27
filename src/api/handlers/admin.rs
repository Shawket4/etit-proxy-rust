//! `/admin/*` — token-gated operational handlers. The auth check is done
//! by the [`crate::api::middleware::admin_guard`] middleware; handlers here
//! assume the request is authorized.

use axum::extract::State;
use axum::Json;

use crate::api::dto::{InvalidateRequest, InvalidateResponse, ReloginResponse, UnblockResponse};
use crate::error::{AppError, AppResult};
use crate::routing::Departure;
use crate::state::AppState;
use crate::upstream::users;

/// `POST /admin/cache/invalidate { "scope": "all" | "vehicles" | "live" | "history" }`
pub async fn invalidate(
    State(s): State<AppState>,
    Json(req): Json<InvalidateRequest>,
) -> AppResult<Json<InvalidateResponse>> {
    match req.scope.as_str() {
        "all" => {
            s.cache.invalidate_all().await;
            // Also wipe persisted history. Vehicles will be re-fetched on
            // the next Tier A tick anyway.
            if let Err(e) = s.history.repo.truncate().await {
                tracing::warn!(error = %e, "history truncate failed");
            }
            Ok(Json(InvalidateResponse {
                scope: "all".into(),
                ok: true,
                message: "all caches and persisted history cleared".into(),
            }))
        }
        "vehicles" => {
            // Force a refresh; the snapshot will be replaced atomically.
            match s.vehicles.refresh_now().await {
                Ok(list) => Ok(Json(InvalidateResponse {
                    scope: "vehicles".into(),
                    ok: true,
                    message: format!("vehicles refreshed; {} vehicles", list.len()),
                })),
                Err(e) => Err(e),
            }
        }
        "live" => {
            match s.live.refresh_now().await {
                Ok(snap) => Ok(Json(InvalidateResponse {
                    scope: "live".into(),
                    ok: true,
                    message: format!("live refreshed; {} vehicles", snap.by_id.len()),
                })),
                Err(e) => Err(e),
            }
        }
        "history" => {
            s.cache.invalidate_all().await;
            if let Err(e) = s.history.repo.truncate().await {
                tracing::warn!(error = %e, "history truncate failed");
            }
            Ok(Json(InvalidateResponse {
                scope: "history".into(),
                ok: true,
                message: "history caches and persisted rows cleared".into(),
            }))
        }
        other => Err(AppError::BadRequest(format!(
            "unknown scope `{other}` (allowed: all, vehicles, live, history)"
        ))),
    }
}

/// `POST /admin/auth/relogin` — force a session re-login.
pub async fn relogin(State(s): State<AppState>) -> AppResult<Json<ReloginResponse>> {
    let epoch = s.client.force_login().await?;
    Ok(Json(ReloginResponse { ok: true, epoch }))
}

/// `POST /admin/users/unblock` — unblock the hardcoded first-row user
/// (`magda.adly`) via the legacy `UserList.aspx` grid postback. Idempotent:
/// unblocking an already-unblocked user is a no-op upstream.
///
/// The response carries best-effort debug detail. `ok: false` with
/// `accessDenied: true` means the acting account lacks rights on UserList.
/// `ok: true` means the server accepted the event with no rejection signals —
/// but the grid markup exposes no blocked flag, so it is not a hard proof the
/// user is now unblocked (see `note`).
pub async fn unblock_user(State(s): State<AppState>) -> AppResult<Json<UnblockResponse>> {
    let outcome = users::unblock_first_row(&s.client).await?;
    Ok(Json(UnblockResponse {
        ok: outcome.accepted(),
        target: users::UNBLOCK_TARGET_LABEL.into(),
        note: outcome.note(),
        http_status: outcome.http_status,
        is_users_grid: outcome.is_users_grid,
        error_page: outcome.error_page,
        access_denied: outcome.access_denied,
        upstream_message: outcome.upstream_message,
        body_excerpt: outcome.body_excerpt,
    }))
}

/// `POST /admin/routing/route` — see [`crate::api::dto::RouteRequest`].
pub async fn route(
    State(s): State<AppState>,
    Json(req): Json<crate::api::dto::RouteRequest>,
) -> AppResult<Json<crate::api::dto::RouteResponse>> {
    let via: Vec<(f64, f64)> = req.via.iter().map(|p| (p[0], p[1])).collect();
    let dep = departure(req.depart_at.as_deref(), req.restricted)?;

    let routed = s
        .routing
        .route((req.from_lat, req.from_lng), (req.to_lat, req.to_lng), &via, dep)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(crate::api::dto::RouteResponse {
        distance_km: routed.distance_km,
        duration_min: routed.duration_secs as f64 / 60.0,
        geometry: routed.geometry,
        restricted: req.restricted,
        departure: describe(dep, &s),
        engine: "valhalla",
    }))
}

/// `POST /admin/routing/optimize` — see [`crate::api::dto::OptimizeRequest`].
pub async fn optimize(
    State(s): State<AppState>,
    Json(req): Json<crate::api::dto::OptimizeRequest>,
) -> AppResult<Json<crate::api::dto::OptimizeResponse>> {
    if req.stops.is_empty() {
        return Err(AppError::BadRequest("stops must not be empty".into()));
    }
    let dep = departure(req.depart_at.as_deref(), req.restricted)?;
    let pt = |p: crate::api::dto::LatLng| (p.lat, p.lng);
    let stops: Vec<(f64, f64)> = req.stops.iter().copied().map(pt).collect();
    let trip = s
        .routing
        .optimize(pt(req.start), &stops, pt(req.end.unwrap_or(req.start)), dep)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(crate::api::dto::OptimizeResponse {
        order: trip.order,
        legs: trip
            .legs
            .into_iter()
            .map(|l| crate::api::dto::OptimizedLeg {
                distance_km: l.distance_km,
                duration_min: l.duration_secs as f64 / 60.0,
                geometry: l.geometry,
            })
            .collect(),
        distance_km: trip.distance_km,
        duration_min: trip.duration_secs as f64 / 60.0,
        geometry: trip.geometry,
        departure: describe(dep, &s),
        engine: "valhalla",
    }))
}

/// `depart_at` wins; without it, `restricted` picks "any hour" or "now".
fn departure(depart_at: Option<&str>, restricted: bool) -> AppResult<Departure> {
    match depart_at.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|d| Departure::At(d.timestamp()))
            .map_err(|e| AppError::BadRequest(format!("depart_at must be RFC 3339: {e}"))),
        None if restricted => Ok(Departure::AnyHour),
        None => Ok(Departure::At(chrono::Utc::now().timestamp())),
    }
}

fn describe(dep: Departure, s: &AppState) -> String {
    match dep {
        Departure::AnyHour => "any_hour".into(),
        Departure::At(ts) => chrono::DateTime::from_timestamp(ts, 0)
            .map(|d| d.with_timezone(&s.cfg.timezone).format("%Y-%m-%dT%H:%M").to_string())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn departure_prefers_an_explicit_time() {
        assert_eq!(departure(Some("2026-11-28T13:00:00+02:00"), true).unwrap(), Departure::At(1795863600));
        assert_eq!(departure(None, true).unwrap(), Departure::AnyHour);
        assert!(matches!(departure(Some("  "), true).unwrap(), Departure::AnyHour));
        assert!(matches!(departure(None, false).unwrap(), Departure::At(_)));
        assert!(departure(Some("tomorrow"), true).is_err());
    }
}
